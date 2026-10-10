// Offline release contract harness. Shell fixture archives are not published-release evidence.
import { strict as assert } from "node:assert";
import {
  createHash,
  createPrivateKey,
  createPublicKey,
  sign,
} from "node:crypto";
import { spawnSync } from "node:child_process";
import {
  chmodSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  realpathSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { delimiter, dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { parseArgs } from "node:util";
import { fixtureArchive } from "../tests/script-support.ts";

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const discovery = "https://tapid.dev/releases/v1/latest.tsv";
const targets = [
  "aarch64-apple-darwin",
  "aarch64-pc-windows-msvc",
  "aarch64-unknown-linux-gnu",
  "x86_64-apple-darwin",
  "x86_64-pc-windows-msvc",
  "x86_64-unknown-linux-gnu",
];
const hash = (bytes: Buffer) =>
  createHash("sha256").update(bytes).digest("hex");
const quote = (value: string) => `'${value.replaceAll("'", "'\\''")}'`;
const fixtureExecutable = (version: string) =>
  Buffer.from(`#!/bin/sh\nprintf 'tapid ${version}\\n'\n`);
const canonical = (value: any): any =>
  Array.isArray(value)
    ? value.map(canonical)
    : value && typeof value === "object"
      ? Object.fromEntries(
          Object.keys(value)
            .sort()
            .map((key) => [key, canonical(value[key])]),
        )
      : value;

// Fixed, public test seed. Never used to sign production releases.
export function signRecord(
  record: string,
  signature: string,
  keyring: string,
): void {
  const privateKey = createPrivateKey({
    key: Buffer.concat([
      Buffer.from("302e020100300506032b657004220420", "hex"),
      Buffer.alloc(32, 7),
    ]),
    format: "der",
    type: "pkcs8",
  });
  const publicBytes = createPublicKey(privateKey)
    .export({ format: "der", type: "spki" })
    .subarray(-32);
  writeFileSync(
    keyring,
    JSON.stringify({
      version: "tapid-release-keyring-v1",
      keys: [
        {
          key_id: "release-key-2026-01",
          algorithm: "ed25519",
          public_key: publicBytes.toString("base64"),
          fingerprint: "sha256-" + hash(publicBytes),
        },
      ],
    }),
  );
  const envelope: any = {
    version: "tapid-trust-envelope-v1",
    subject: "tapid-release-v1",
    artifact_digest: "sha256-" + hash(readFileSync(record)),
    claims: {
      schema: "tapid-release-v1-immutable-signature",
      created_at: new Date(Date.now() - 3650 * 86400000).toISOString(),
    },
  };
  const payload = Buffer.from(
    JSON.stringify(
      canonical({
        ...envelope,
        signature_context: {
          algorithm: "ed25519",
          key_id: "release-key-2026-01",
        },
      }),
    ),
  );
  writeFileSync(join(dirname(record), ".signature-payload.json"), payload);
  const raw = sign(null, payload, privateKey);
  writeFileSync(join(dirname(record), ".signature.raw"), raw);
  envelope.signature = {
    algorithm: "ed25519",
    key_id: "release-key-2026-01",
    subject: envelope.subject,
    artifact_digest: envelope.artifact_digest,
    value: raw.toString("base64"),
  };
  writeFileSync(signature, JSON.stringify(envelope));
}
function command(
  args: string[],
  env: NodeJS.ProcessEnv,
  cwd: string,
  fails = false,
): string {
  const result = spawnSync(args[0], args.slice(1), {
    cwd,
    env,
    encoding: "utf8",
    timeout: 60_000,
    maxBuffer: 65536,
  });
  assert.ifError(result.error);
  const output = result.stdout + result.stderr;
  assert.ok(
    Buffer.byteLength(output) <= 65536,
    "command output exceeded 64 KiB",
  );
  assert.equal(
    result.status !== 0,
    fails,
    `unexpected exit ${result.status}: ${args[0]}\n${output}`,
  );
  return output;
}
function release(
  directory: string,
  version: string,
  provider: string,
  env: NodeJS.ProcessEnv,
  keyring: string,
) {
  mkdirSync(directory);
  const payload = fixtureExecutable(version);
  for (const target of targets)
    writeFileSync(
      join(directory, `tapid-${version}-${target}.tar.gz`),
      fixtureArchive(
        target.includes("windows") ? "tapid.exe" : "tapid",
        payload,
        0o755,
      ),
    );
  command(
    [
      process.execPath,
      "--experimental-strip-types",
      join(root, "tools/release/release.ts"),
      "metadata",
      directory,
      version,
      provider,
    ],
    env,
    directory,
  );
  const record = join(directory, "tapid-release-v1.tsv"),
    signature = record + ".sig";
  signRecord(record, signature, keyring);
  const mapping: Record<string, string> = {
    [discovery]: record,
    [discovery + ".sig"]: signature,
    [`https://tapid.dev/releases/v1/v${version}.tsv`]: record,
    [`https://tapid.dev/releases/v1/v${version}.tsv.sig`]: signature,
  };
  for (const name of readdirSync(directory).filter((name) =>
    name.endsWith(".tar.gz"),
  ))
    mapping[`${provider}/${name}`] = join(directory, name);
  return { mapping, payload };
}
const snapshot = (directory: string) =>
  Object.fromEntries(
    readdirSync(directory, { withFileTypes: true })
      .filter((entry) => entry.isFile())
      .map((entry) => [entry.name, readFileSync(join(directory, entry.name))]),
  );
export function check(binary: string): void {
  assert.ok(
    ["linux", "darwin"].includes(process.platform),
    "this integration check requires Unix",
  );
  assert.ok(existsSync(binary), `missing source-built CLI: ${binary}`);
  const directory = mkdtempSync(join(tmpdir(), "tapid-release-contract-"));
  try {
    const transport = join(directory, "transport"),
      home = join(directory, "home");
    mkdirSync(transport);
    mkdirSync(home);
    const env: NodeJS.ProcessEnv = {
      PATH: transport + delimiter + process.env.PATH,
      HOME: home,
      TMPDIR: directory,
      LC_ALL: "C",
      RELEASE_FIXTURE: directory,
    };
    const curl = join(transport, "curl");
    // Exact local URL mapping. Unknown URLs fail closed and never reach a network.
    writeFileSync(
      curl,
      `#!${process.execPath}
const fs = require('node:fs'), path = require('node:path');
const root = process.env.RELEASE_FIXTURE, args = process.argv.slice(2), urls = args.filter(arg => arg.startsWith('https://'));
if (urls.length !== 1) process.exit(22);
const url = urls[0]; fs.appendFileSync(path.join(root, 'requests'), url + '\\n');
const mapping = JSON.parse(fs.readFileSync(path.join(root, 'mapping.json'), 'utf8'));
if (!Object.hasOwn(mapping, url)) process.exit(22);
const body = fs.readFileSync(mapping[url]);
if (args.includes('--max-filesize') && body.length > Number(args[args.indexOf('--max-filesize') + 1])) process.exit(63);
if (args.includes('-o')) fs.writeFileSync(args[args.indexOf('-o') + 1], body); else process.stdout.write(body);
`,
    );
    chmodSync(curl, 0o755);
    const installed = join(directory, "installed");
    mkdirSync(installed);
    const destination = join(installed, "tapid"),
      keyring = join(directory, "keyring.json");
    const first = release(
      join(directory, "first"),
      "1.2.3",
      "https://gitlab.example/tapid/releases/v1.2.3/downloads",
      env,
      keyring,
    );
    const bootstrap = join(directory, "bootstrap");
    mkdirSync(bootstrap);
    const provider = "https://fixture.example/bootstrap/v1.0.0";
    const wrapper = Buffer.from(
      `#!/bin/sh\nif [ "$1" = __verify-release-record ]; then\n  exec ${quote(binary)} "$@" --keyring ${quote(keyring)}\nfi\nexec ${quote(binary)} "$@"\n`,
    );
    const bootstrapMapping: Record<string, string> = {};
    for (const target of targets) {
      const name = `tapid-1.0.0-${target}.tar.gz`,
        path = join(bootstrap, name);
      writeFileSync(
        path,
        fixtureArchive(
          target.includes("windows") ? "tapid.exe" : "tapid",
          wrapper,
          0o755,
        ),
      );
      bootstrapMapping[`${provider}/${name}`] = path;
    }
    command(
      [
        process.execPath,
        "--experimental-strip-types",
        join(root, "tools/release/bootstrap.ts"),
        bootstrap,
        "1.0.0",
        provider,
      ],
      env,
      directory,
    );
    const installer = join(bootstrap, "install.sh"),
      mappingPath = join(directory, "mapping.json");
    writeFileSync(
      mappingPath,
      JSON.stringify({ ...first.mapping, ...bootstrapMapping }),
    );
    command(["sh", installer, "--install-dir", installed], env, directory);
    assert.deepEqual(
      readFileSync(destination),
      first.payload,
      "installer changed executable bytes",
    );
    assert.equal(
      command([destination, "--version"], env, directory).trim(),
      "tapid 1.2.3",
    );
    command(
      [
        "sh",
        installer,
        "--version",
        "1.2.3",
        "--install-dir",
        join(directory, "explicit"),
      ],
      env,
      directory,
    );
    env.TAPID_RELEASE_KEYRING = keyring;
    writeFileSync(destination, fixtureExecutable("1.2.2"));
    writeFileSync(join(installed, ".tapid-managed"), "tapid-managed-v1\n");
    const upgrade = [binary, "upgrade", "--destination", destination];
    assert.ok(
      command(upgrade, env, directory).includes("Upgraded Tapid to 1.2.3"),
    );
    assert.deepEqual(
      readFileSync(destination),
      first.payload,
      "updater and installer disagree on artifact bytes",
    );
    let before = snapshot(installed);
    assert.ok(
      command(upgrade, env, directory).includes(
        "Tapid 1.2.3 is already up to date",
      ),
    );
    assert.deepEqual(snapshot(installed), before);
    const second = release(
      join(directory, "second"),
      "1.2.4",
      "https://downloads.example.net/tapid/v1.2.4",
      env,
      keyring,
    );
    writeFileSync(
      mappingPath,
      JSON.stringify({ ...second.mapping, ...bootstrapMapping }),
    );
    assert.ok(
      command(upgrade, env, directory).includes("Upgraded Tapid to 1.2.4"),
    );
    assert.deepEqual(readFileSync(destination), second.payload);
    assert.equal(
      command([destination, "--version"], env, directory).trim(),
      "tapid 1.2.4",
    );
    before = snapshot(installed);
    assert.ok(
      command(upgrade, env, directory).includes(
        "Tapid 1.2.4 is already up to date",
      ),
    );
    assert.deepEqual(snapshot(installed), before);
    const state = JSON.parse(
      readFileSync(join(installed, ".tapid-release-state.json"), "utf8"),
    );
    assert.equal(state.verification, "signature");
    assert.equal(state.last_known_good.version, "1.2.4");
    const digest = state.last_known_good.artifact_sha256,
      cache = join(installed, ".tapid-release-artifact-" + digest);
    assert.equal(
      hash(readFileSync(cache)),
      digest,
      "no valid cached release available for the fail-closed check",
    );
    const record = join(directory, "second/tapid-release-v1.tsv");
    writeFileSync(
      record,
      readFileSync(record, "utf8").replace(
        "tapid-release-v1",
        "tapid-release-v99",
      ),
    );
    signRecord(record, record + ".sig", keyring);
    assert.ok(
      command(upgrade, env, directory, true).includes(
        "invalid tapid-release-v1 record",
      ),
    );
    assert.deepEqual(
      snapshot(installed),
      before,
      "rejected metadata changed installation or recovery state",
    );
    const requests = readFileSync(join(directory, "requests"), "utf8")
      .trim()
      .split("\n");
    assert.equal(requests.filter((url) => url === discovery).length, 6);
    assert.equal(
      requests.filter((url) => url === discovery + ".sig").length,
      6,
    );
    assert.ok(
      requests.some((url) => url.startsWith("https://gitlab.example/")),
    );
    assert.ok(
      requests.some((url) => url.startsWith("https://downloads.example.net/")),
    );
    assert.ok(requests.every((url) => new URL(url).hostname !== "github.com"));
    console.log(
      "Generated signed release record passed upgrade, repeat, provider migration, and cached rejection checks.",
    );
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
}
function main(): void {
  const { values } = parseArgs({
    options: {
      binary: { type: "string" },
      "sign-record": { type: "string" },
      signature: { type: "string" },
      keyring: { type: "string" },
    },
  });
  if (values["sign-record"]) {
    if (!values.signature || !values.keyring)
      throw new Error("--sign-record requires --signature and --keyring");
    signRecord(values["sign-record"], values.signature, values.keyring);
  } else {
    if (!values.binary) throw new Error("--binary is required");
    check(realpathSync(values.binary));
  }
}
if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(resolve(process.argv[1])).href
)
  main();
