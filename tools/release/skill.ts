// A documentation-only, deterministic ZIP. Verification compares all bytes with
// regeneration from trusted source; untrusted ZIP entries are never extracted.
import { createHash } from 'node:crypto';
import { posix, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { lstat, writeFile } from 'node:fs/promises';
import { compareStableVersions } from './versions.ts';
import { releaseVersion } from './release.ts';

export type SkillIdentity = { tag: string; commit: string; cliVersion: string; schemaVersion: number };
export type ReadSource = (path: string) => Promise<string>;
export function skillAsset(tag: string): string[] {
  releaseVersion(tag,tag.slice(1));
  return compareStableVersions(tag.slice(1),'0.0.13')! > 0 ? [`tapid-skill-${tag.slice(1)}.zip`] : [];
}
const documents = ['agent-guide.md', 'json-results.md', 'compatibility.md', 'npm-lockfile-import.md', 'dependency-lifecycle.md'];
const maximum = 2 * 1024 * 1024;
const hash = (text: string) => createHash('sha256').update(text).digest('hex');

function anchors(text: string): Set<string> {
  const seen = new Map<string, number>();
  return new Set([...text.matchAll(/^#{1,6} +(.+)$/gm)].map(match => {
    const slug = match[1].toLowerCase().replace(/[^\p{L}\p{N}_\-\s]/gu, '').replace(/ /g, '-');
    const count = seen.get(slug) ?? 0; seen.set(slug, count + 1);
    return count ? `${slug}-${count}` : slug;
  }));
}

async function references(text: string, path: string, read: ReadSource, identity: SkillIdentity): Promise<string> {
  const links = [...text.matchAll(/\]\(([^\s)]+)\)/g)];
  let result = text;
  for (const match of links.reverse()) {
    const link = match[1];
    if (/^https:\/\//.test(link)) continue;
    const [file, fragment] = link.split('#');
    const target = posix.normalize(posix.join(posix.dirname(path), file || posix.basename(path)));
    if (target.startsWith('../') || target.startsWith('/') || /[\\?%]/.test(target)) throw Error(`unsafe reference: ${link}`);
    const content = target === path ? text : await read(target);
    if (fragment && !anchors(content).has(fragment)) throw Error(`missing anchor: ${link}`);
    const bundled = target.startsWith('docs/') && documents.includes(target.slice(5));
    const replacement = bundled ? `${posix.basename(target)}${fragment ? '#' + fragment : ''}` :
      `https://github.com/LimeTip/tapid/blob/${identity.commit}/${target}${fragment ? '#' + fragment : ''}`;
    const offset = match.index! + 2;
    result = result.slice(0, offset) + replacement + result.slice(offset + link.length);
  }
  return result;
}

// Stored entries avoid platform compressor variance; fixed DOS epoch, regular
// non-executable mode and a closed member set make the emitted archive portable.
function zip(files: Record<string, string>): Buffer {
  const local: Buffer[] = [], central: Buffer[] = []; let offset = 0;
  for (const [path, text] of Object.entries(files).sort(([a], [b]) => a < b ? -1 : 1)) {
    const name = Buffer.from(`tapid/${path}`), data = Buffer.from(text);
    let crc = 0xffffffff;
    for (const byte of data) { crc ^= byte; for (let bit = 0; bit < 8; bit++) crc = (crc >>> 1) ^ (crc & 1 ? 0xedb88320 : 0); }
    crc = (crc ^ 0xffffffff) >>> 0;
    const header = Buffer.alloc(30);
    header.writeUInt32LE(0x04034b50); header.writeUInt16LE(20, 4); header.writeUInt16LE(0x800, 6);
    header.writeUInt16LE(33, 12); header.writeUInt32LE(crc, 14);
    header.writeUInt32LE(data.length, 18); header.writeUInt32LE(data.length, 22); header.writeUInt16LE(name.length, 26);
    local.push(header, name, data);
    const record = Buffer.alloc(46);
    record.writeUInt32LE(0x02014b50); record.writeUInt16LE(0x314, 4); header.copy(record, 6, 4, 30);
    record.writeUInt32LE((0o100644 * 65536) >>> 0, 38); record.writeUInt32LE(offset, 42);
    central.push(record, name); offset += header.length + name.length + data.length;
  }
  const directory = Buffer.concat(central), end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50); end.writeUInt16LE(central.length / 2, 8); end.writeUInt16LE(central.length / 2, 10);
  end.writeUInt32LE(directory.length, 12); end.writeUInt32LE(offset, 16);
  return Buffer.concat([...local, directory, end]);
}

export async function buildSkill(source: ReadSource, identity: SkillIdentity): Promise<Buffer> {
  const read: ReadSource = async path => {
    const text = await source(path);
    if (Buffer.byteLength(text) > maximum || text.includes('\0')) throw Error('invalid or oversized skill source');
    return text;
  };
  releaseVersion(identity.tag, identity.cliVersion);
  if (!/^[a-f0-9]{40}$/.test(identity.commit) || identity.schemaVersion !== 1) throw Error('invalid skill identity');
  const compatibilityText = await read('skills/tapid/compatibility.json');
  const compatibility = JSON.parse(compatibilityText);
  if (compatibility.schema_version !== 1 || compatibility.json_schema_version !== identity.schemaVersion ||
      !Array.isArray(compatibility.supported_cli_versions) ||
      !compatibility.supported_cli_versions.includes(identity.cliVersion)) throw Error('unreviewed CLI compatibility');
  const cargo = await read('crates/tapid-cli/Cargo.toml');
  const packageSection = cargo.match(/\[package\]([\s\S]*?)(?=\n\[|$)/)?.[1];
  const packageVersion = packageSection?.match(/^version\s*=\s*"([^"]+)"\s*(?:#.*)?$/m)?.[1];
  if (packageVersion !== identity.cliVersion) throw Error('CLI source version mismatch');
  const entry = await read('skills/tapid/SKILL.md');
  // The authored header uses a deliberately small, valid YAML plain-scalar subset.
  if (!/^---\nname: tapid\ndescription: [A-Za-z][A-Za-z0-9 ,.'()/-]{0,1023}\nlicense: Apache-2.0\n---\n/.test(entry) || entry.split('\n').length >= 500) throw Error('invalid skill frontmatter or size');
  const files: Record<string, string> = { 'SKILL.md': entry, 'LICENSE': await read('LICENSE') };
  const sources: Record<string, string> = {};
  for (const name of documents) {
    const path = `docs/${name}`, text = await read(path);
    if (Buffer.byteLength(text) > maximum) throw Error('skill source exceeds maximum size');
    sources[path] = hash(text);
    files[`references/${name}`] = await references(text, path, read, identity);
  }
  sources['crates/tapid-cli/Cargo.toml'] = hash(cargo);
  sources['skills/tapid/compatibility.json'] = hash(compatibilityText);
  sources['skills/tapid/SKILL.md'] = hash(entry); sources.LICENSE = hash(files.LICENSE);
  for (const [, target] of entry.matchAll(/\]\(([^\s)]+)\)/g)) {
    const [path, fragment] = target.split('#');
    if (path === 'manifest.json' && !fragment) continue;
    if (!(path in files)) throw Error(`missing bundled reference: ${target}`);
    if (fragment && !anchors(files[path]).has(fragment)) throw Error(`missing bundled anchor: ${target}`);
  }
  files['manifest.json'] = JSON.stringify({ format_version: 1, release_tag: identity.tag, source_commit: identity.commit,
    source_url: `https://github.com/LimeTip/tapid/tree/${identity.commit}`, supported_cli_versions: [identity.cliVersion],
    json_schema_version: identity.schemaVersion, compatibility_evidence: compatibility, sources,
    files: Object.fromEntries(Object.entries(files).map(([path, content]) => [path, hash(content)])),
  }, null, 2) + '\n';
  const bytes = zip(files);
  if (bytes.length > maximum) throw Error('skill archive exceeds maximum size');
  return bytes;
}

export async function verifySkill(bytes: Buffer, read: ReadSource, identity: SkillIdentity): Promise<void> {
  if (bytes.length > maximum || !bytes.equals(await buildSkill(read, identity))) throw Error('skill differs from trusted source regeneration');
}

export function sourceAtCommit(commit: string): ReadSource {
  if (!/^[a-f0-9]{40}$/.test(commit)) throw Error('invalid source commit');
  return async path => {
    if (!/^[A-Za-z0-9._/-]+$/.test(path) || path.split('/').includes('..')) throw Error('unsafe source path');
    const { stdout } = await promisify(execFile)('git', ['show', `${commit}:${path}`], { encoding: 'buffer', maxBuffer: maximum, timeout: 60000 });
    return new TextDecoder('utf-8', { fatal: true }).decode(stdout);
  };
}

if (process.argv[1] && pathToFileURL(resolve(process.argv[1])).href === import.meta.url) {
  const [command, file, tag, commit] = process.argv.slice(2);
  if (!['build', 'verify', 'build-release', 'verify-release'].includes(command) || !file || !tag || !commit || process.argv.length !== 6) throw Error('usage: skill.ts build|verify ZIP TAG SOURCE_COMMIT (or build-release|verify-release DIRECTORY TAG SOURCE_COMMIT)');
  const identity = { tag, commit, cliVersion: tag.slice(1), schemaVersion: 1 };
  const read = sourceAtCommit(commit);
  const release = command.endsWith('-release');
  const assets = release ? skillAsset(tag) : [file];
  if (release && assets.length === 0) process.exit(0);
  const destination = release ? resolve(file, assets[0]) : file;
  if (command.startsWith('build')) await writeFile(destination, await buildSkill(read, identity), { flag: 'wx' });
  else {
    const { readBounded } = await import('../../scripts/check-doc-examples.ts');
    if (!(await lstat(destination)).isFile()) throw Error('skill archive must be a regular file');
    await verifySkill(readBounded(destination, maximum), read, identity);
  }
}
