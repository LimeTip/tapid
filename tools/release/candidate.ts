import { createHash } from 'node:crypto';
import { createReadStream } from 'node:fs';
import { copyFile, lstat, mkdtemp, readdir, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { checksumLines, releaseRecord, releaseVersion } from './release.ts';
import { renderInstallers, templatesForCommit, type InstallerTemplates } from './bootstrap.ts';

export type Asset = { id: number; name: string; size: number };
export type ReleaseState = { id: number; tag_name: string; name: string; body: string; draft: boolean; prerelease: boolean; immutable?: boolean; assets: Asset[] };
export type ReleaseAdapter = {
  repository: string;
  tag(tag: string): Promise<{ object: string; commit: string } | null>;
  isOnMain(commit: string): Promise<boolean>;
  createTag(tag: string, commit: string): Promise<void>;
  releases(tag: string): Promise<ReleaseState[]>;
  release(id: number): Promise<ReleaseState>;
  latest(): Promise<ReleaseState | null>;
  download(asset: Asset, path: string): Promise<void>;
  promote(id: number, latest: boolean): Promise<void>;
  createRelease(tag: string, notes: string): Promise<ReleaseState>;
  upload(id: number, name: string, path: string): Promise<void>;
};
export type ReleaseIntent = { schema: 'tapid-release-intent-v1'; version: string; baseline: string; prepared_from:string; notes: string; packages: {name:string;version:string}[] };
export type Candidate = {
  schema: 'tapid-release-candidate-v1'; repository: string; tag: string; commit: string; tag_object: string;
  release_id: number; title: string; notes_sha256: string; plan_sha256: string;
  assets: (Asset & {sha256:string})[];
};
export type UnsignedCandidate = Omit<Candidate,'schema'|'release_id'|'assets'> & {
  schema:'tapid-release-unsigned-candidate-v1'; assets:{name:string;size:number;sha256:string}[];
};
const targets = ['aarch64-apple-darwin','aarch64-pc-windows-msvc','aarch64-unknown-linux-gnu','x86_64-apple-darwin','x86_64-pc-windows-msvc','x86_64-unknown-linux-gnu'];
function keys(value: unknown, expected: string[]): asserts value is Record<string,unknown> {
  if (!value || typeof value !== 'object' || Array.isArray(value) || JSON.stringify(Object.keys(value).sort()) !== JSON.stringify([...expected].sort())) throw Error('unexpected release fields');
}
function version(value: unknown): asserts value is string {
  if (typeof value !== 'string') throw Error('invalid release version');
  releaseVersion(`v${value}`, value);
}
function compare(left: string, right: string): number {
  version(left); version(right);
  const a=left.split('.').map(BigInt),b=right.split('.').map(BigInt);
  for(let i=0;i<3;i++) if(a[i]!==b[i]) return a[i]>b[i] ? 1 : -1;
  return 0;
}
function source(tag: string, commit: string): void {
  releaseVersion(tag,tag.slice(1));
  if(!/^[a-f0-9]{40}$/.test(commit)) throw Error('invalid source commit');
}
function numeric(value: number): void { if(!Number.isSafeInteger(value) || value<=0) throw Error('invalid numeric release or asset ID'); }
function repository(value: string): void { if(!/^[A-Za-z0-9][A-Za-z0-9_-]*\/[A-Za-z0-9][A-Za-z0-9._-]*$/.test(value)) throw Error('invalid repository'); }
export function validateReleaseIntent(input: unknown): ReleaseIntent {
  keys(input,['schema','version','baseline','prepared_from','notes','packages']);
  if(input.schema!=='tapid-release-intent-v1') throw Error('invalid release intent schema');
  version(input.version);
  if(typeof input.baseline!=='string') throw Error('invalid release baseline');
  releaseVersion(input.baseline,input.baseline.slice(1));
  if(compare(input.version,input.baseline.slice(1))<=0) throw Error('release must advance baseline');
  if(typeof input.prepared_from!=='string' || !/^[a-f0-9]{40}$/.test(input.prepared_from)) throw Error('invalid prepared source commit');
  if(input.notes!==`docs/releases/${input.version}.md`) throw Error('release notes must have canonical path');
  if(!Array.isArray(input.packages) || input.packages.length===0 || input.packages.length>100) throw Error('invalid publication packages');
  const names=new Set<string>();
  for(const pkg of input.packages) {
    keys(pkg,['name','version']); version(pkg.version);
    if(typeof pkg.name!=='string' || !/^[a-z][a-z0-9_-]*$/.test(pkg.name) || names.has(pkg.name)) throw Error('invalid or duplicate package');
    names.add(pkg.name);
  }
  const tapid=input.packages.find(pkg=>pkg.name==='tapid');
  if(!tapid || tapid.version!==input.version || input.packages.at(-1)?.name!=='tapid') throw Error('tapid must be last at product version');
  return input as ReleaseIntent;
}
export function expectedAssets(tag: string): string[] {
  source(tag,'0'.repeat(40));
  return [...targets.map(t=>`tapid-${tag.slice(1)}-${t}.tar.gz`),'SHA256SUMS','tapid-release-v1.tsv','tapid-release-v1.tsv.sig','install.sh','install.ps1'].sort();
}
export async function selectCandidate(api: ReleaseAdapter, tag: string, commit: string) {
  source(tag,commit); repository(api.repository);
  if(!await api.isOnMain(commit)) throw Error('release source must belong to protected main');
  let ref=await api.tag(tag);
  if(!ref) { await api.createTag(tag,commit); ref=await api.tag(tag); }
  if(!ref || !/^[a-f0-9]{40}$/.test(ref.object) || ref.commit!==commit) throw Error('annotated release tag source mismatch');
  const releases=await api.releases(tag);
  if(releases.length>1 || releases.some(r=>r.tag_name!==tag)) throw Error('ambiguous matching release');
  if(releases[0]) numeric(releases[0].id);
  return {tag,commit,tag_object:ref.object,release_id:releases[0]?.id ?? null,draft:releases[0]?.draft ?? null};
}
const digest = (value: string | Buffer) => createHash('sha256').update(value).digest('hex');
function canonical(value: unknown): unknown {
  if(Array.isArray(value)) return value.map(canonical);
  if(value && typeof value==='object') return Object.fromEntries(Object.entries(value).sort(([a],[b])=>a<b?-1:a>b?1:0).map(([key,item])=>[key,canonical(item)]));
  return value;
}
export function candidateFingerprint(candidate: Candidate | UnsignedCandidate): string {
  const valid=candidate.schema==='tapid-release-unsigned-candidate-v1' ? validateUnsignedCandidate(candidate) : validateCandidate(candidate);
  return digest(JSON.stringify(canonical(valid)));
}
export function validateCandidate(value: unknown): Candidate {
  keys(value,['schema','repository','tag','commit','tag_object','release_id','title','notes_sha256','plan_sha256','assets']);
  if(value.schema!=='tapid-release-candidate-v1') throw Error('invalid candidate schema');
  if(typeof value.tag!=='string' || typeof value.commit!=='string' || typeof value.repository!=='string') throw Error('invalid candidate identity');
  source(value.tag,value.commit); repository(value.repository);
  if(typeof value.tag_object!=='string' || !/^[a-f0-9]{40}$/.test(value.tag_object) || value.title!==value.tag) throw Error('invalid candidate tag object or title');
  numeric(value.release_id as number);
  for(const name of ['notes_sha256','plan_sha256']) if(typeof value[name]!=='string' || !/^[a-f0-9]{64}$/.test(value[name] as string)) throw Error('invalid candidate digest');
  if(!Array.isArray(value.assets) || value.assets.length!==11) throw Error('invalid candidate asset set');
  const ids=new Set<number>();
  for(const asset of value.assets) {
    keys(asset,['id','name','size','sha256']); numeric(asset.id as number);
    if(ids.has(asset.id as number)) throw Error('duplicate asset ID'); ids.add(asset.id as number);
    if(typeof asset.name!=='string' || typeof asset.sha256!=='string' || !/^[a-f0-9]{64}$/.test(asset.sha256)) throw Error('invalid asset digest');
    if(!Number.isSafeInteger(asset.size) || (asset.size as number)<=0 || (asset.size as number)>limit(asset.name)) throw Error('invalid asset size');
  }
  if(JSON.stringify(value.assets.map(a=>a.name))!==JSON.stringify(expectedAssets(value.tag))) throw Error('invalid candidate asset names or order');
  return value as Candidate;
}
function limit(name: string): number { return name.endsWith('.tar.gz') ? 512*1024*1024 : 256*1024; }
async function fileDigest(path: string, max: number): Promise<{sha256:string;size:number}> {
  const info=await lstat(path);
  if(!info.isFile() || info.size<=0 || info.size>max) throw Error(`invalid regular file: ${path}`);
  const hash=createHash('sha256');let size=0;
  for await(const chunk of createReadStream(path)) { size+=chunk.length;if(size>max) throw Error('asset exceeds maximum size');hash.update(chunk); }
  if(size!==info.size) throw Error('asset changed while hashing');
  return {sha256:hash.digest('hex'),size};
}
async function smallFile(path: string): Promise<string> {
  if(!(await lstat(path)).isFile())throw Error('candidate input must be a regular file');
  const chunks:Buffer[]=[];let size=0;
  for await(const chunk of createReadStream(path)){size+=chunk.length;if(size>1024*1024)throw Error('candidate input exceeds maximum size');chunks.push(chunk);}
  const bytes=Buffer.concat(chunks),value=bytes.toString('utf8');
  if(!bytes.length || !Buffer.from(value).equals(bytes))throw Error('candidate input must be nonempty UTF-8');
  return value;
}
export function unwrapCandidate(value:unknown):Candidate|UnsignedCandidate {
  if(value && typeof value==='object' && 'candidate' in value){
    const envelope=value as Record<string,unknown>;
    if(Object.keys(envelope).some(key=>!['candidate','candidate_sha256','draft','release_id'].includes(key)))throw Error('unexpected candidate envelope fields');
    const candidate=unwrapCandidate(envelope.candidate);
    if(envelope.candidate_sha256!==candidateFingerprint(candidate))throw Error('candidate envelope digest mismatch');
    if('draft' in envelope && typeof envelope.draft!=='boolean')throw Error('invalid envelope draft state');
    if('release_id' in envelope && (!('release_id' in candidate) || envelope.release_id!==candidate.release_id))throw Error('envelope release ID mismatch');
    return candidate;
  }
  return value && typeof value==='object' && 'schema' in value && value.schema==='tapid-release-unsigned-candidate-v1' ? validateUnsignedCandidate(value) : validateCandidate(value);
}
export function validateUnsignedCandidate(value: unknown): UnsignedCandidate {
  keys(value,['schema','repository','tag','commit','tag_object','title','notes_sha256','plan_sha256','assets']);
  if(value.schema!=='tapid-release-unsigned-candidate-v1' || !Array.isArray(value.assets)) throw Error('invalid unsigned candidate');
  const synthetic=value.assets.map((asset,index)=>{
    keys(asset,['name','size','sha256']); return {...asset,id:index+1};
  });
  synthetic.push({name:'tapid-release-v1.tsv.sig',size:1,sha256:'0'.repeat(64),id:11});
  synthetic.sort((a,b)=>String(a.name)<String(b.name)?-1:1);
  validateCandidate({...value,schema:'tapid-release-candidate-v1',release_id:1,assets:synthetic});
  return value as UnsignedCandidate;
}
export async function attestUnsignedCandidate(directory:string,notes:string,plan:string,repo:string,tag:string,commit:string,tagObject:string,templates?:InstallerTemplates):Promise<UnsignedCandidate> {
  source(tag,commit);repository(repo);
  const names=expectedAssets(tag).filter(name=>!name.endsWith('.sig'));
  const local=(await readdir(directory)).filter(name=>name!=='tapid-release-v1.tsv.sig').sort();
  if(JSON.stringify(local)!==JSON.stringify(names)) throw Error('unsigned candidate must contain exactly ten files');
  const assets=[];
  for(const name of names) assets.push({name,...await fileDigest(join(directory,name),limit(name))});
  const base=`https://github.com/${repo}/releases/download/${tag}`;
  if(await smallFile(join(directory,'SHA256SUMS'))!==await checksumLines(directory,tag.slice(1))) throw Error('archive checksum mismatch');
  if(await smallFile(join(directory,'tapid-release-v1.tsv'))!==await releaseRecord(directory,tag.slice(1),base)) throw Error('release record mismatch');
  const regenerated=await mkdtemp(join(tmpdir(),'tapid-installers-'));
  try {
    for(const name of names.filter(n=>n.endsWith('.tar.gz'))) await copyFile(join(directory,name),join(regenerated,name));
    await renderInstallers(regenerated,tag.slice(1),base,templates);
    for(const name of ['install.sh','install.ps1']) if(await smallFile(join(directory,name))!==await smallFile(join(regenerated,name))) throw Error('generated installer mismatch');
  }finally{await rm(regenerated,{recursive:true,force:true});}
  return validateUnsignedCandidate({schema:'tapid-release-unsigned-candidate-v1',repository:repo,tag,commit,tag_object:tagObject,title:tag,notes_sha256:digest(await smallFile(notes)),plan_sha256:digest(await smallFile(plan)),assets});
}
export async function validateUnsignedFiles(directory:string,notes:string,plan:string,input:UnsignedCandidate,templates?:InstallerTemplates):Promise<string> {
  const candidate=validateUnsignedCandidate(input);
  const actual=await attestUnsignedCandidate(directory,notes,plan,candidate.repository,candidate.tag,candidate.commit,candidate.tag_object,templates);
  if(candidateFingerprint(candidate)!==candidateFingerprint(actual)) throw Error('unsigned candidate changed after approval');
  return candidateFingerprint(actual);
}
export async function downloadCandidate(api:ReleaseAdapter,id:number,directory:string,tag:string,commit:string):Promise<void> {
  const selected=await selectCandidate({...api,createTag:async()=>{throw Error('release tag is missing');}},tag,commit);
  if(selected.release_id!==id) throw Error('numeric release identity mismatch');
  const state=await api.release(id);
  if(state.id!==id || state.tag_name!==tag || state.name!==tag || state.prerelease || (!state.draft && !state.immutable)) throw Error('release identity or immutability mismatch');
  if(JSON.stringify(state.assets.map(asset=>asset.name).sort())!==JSON.stringify(expectedAssets(tag))) throw Error('incomplete release: rerun the failed upload job using its original retained build artifacts');
  if((await readdir(directory)).length!==0) throw Error('download destination must be empty');
  const ids=new Set<number>();
  for(const asset of state.assets){numeric(asset.id);if(ids.has(asset.id))throw Error('duplicate asset ID');ids.add(asset.id);if(!Number.isSafeInteger(asset.size)||asset.size<=0||asset.size>limit(asset.name))throw Error('invalid asset size');}
  for(const asset of state.assets) await api.download(asset,join(directory,asset.name));
}
export async function createDraftCandidate(api:ReleaseAdapter,directory:string,notes:string,plan:string,input:UnsignedCandidate,templates?:InstallerTemplates) {
  const candidate=validateUnsignedCandidate(input);
  if(candidate.repository!==api.repository) throw Error('candidate repository mismatch');
  await validateUnsignedFiles(directory,notes,plan,candidate,templates);
  await fileDigest(join(directory,'tapid-release-v1.tsv.sig'),256*1024);
  const selected=await selectCandidate({...api,createTag:async()=>{throw Error('release tag is missing');}},candidate.tag,candidate.commit);
  if(selected.tag_object!==candidate.tag_object) throw Error('annotated tag object changed after approval');
  let id=selected.release_id;
  if(id===null) {
    const created=await api.createRelease(candidate.tag,await smallFile(notes));numeric(created.id);id=created.id;
    // GitHub's collection read-back can lag creation. No mutation is retried here.
    for(let attempt=0;attempt<5;attempt++) {
      const matches=await api.releases(candidate.tag);
      if(matches.length>1 || matches.some(r=>r.id!==id)) throw Error('release collection identity mismatch');
      if(matches.length===1) break;
      if(attempt===4) throw Error('draft creation read-back did not converge');
      await new Promise(resolve=>setTimeout(resolve,200*(attempt+1)));
    }
  }
  const state=await api.release(id);
  if(state.id!==id || state.tag_name!==candidate.tag || state.name!==candidate.tag || state.prerelease || state.body!==await smallFile(notes)) throw Error('existing draft identity or notes mismatch');
  const names=expectedAssets(candidate.tag);
  const seen=new Set<string>(),ids=new Set<number>();
  for(const asset of state.assets) {
    if(!names.includes(asset.name) || seen.has(asset.name)) throw Error('unexpected existing release assets');seen.add(asset.name);numeric(asset.id);
    if(ids.has(asset.id)||!Number.isSafeInteger(asset.size)||asset.size<=0||asset.size>limit(asset.name))throw Error('invalid existing release asset');ids.add(asset.id);
  }
  if(!state.draft && (state.immutable!==true || state.assets.length!==11)) throw Error('existing public release must be complete and immutable');
  const downloaded=await mkdtemp(join(tmpdir(),'tapid-draft-readback-'));
  try {
    // Check every existing asset before the first upload. Never overwrite bytes.
    for(const asset of state.assets) {
      await api.download(asset,join(downloaded,asset.name));
      const existing=await fileDigest(join(downloaded,asset.name),limit(asset.name));
      const expected=await fileDigest(join(directory,asset.name),limit(asset.name));
      if(existing.size!==asset.size || JSON.stringify(existing)!==JSON.stringify(expected)) throw Error('existing draft asset differs from approved bytes');
    }
    for(const name of names) if(!seen.has(name)) await api.upload(id,name,join(directory,name));
    for(let attempt=0;attempt<5;attempt++) {
      const uploaded=await api.release(id);
      const uploadedNames=uploaded.assets.map(asset=>asset.name).sort();
      if(JSON.stringify(uploadedNames)===JSON.stringify(names)) break;
      if(uploadedNames.some(name=>!names.includes(name)))throw Error('unexpected uploaded assets');
      if(attempt===4)throw Error('draft assets read-back did not converge');
      await new Promise(resolve=>setTimeout(resolve,200*(attempt+1)));
    }
    const signed=await attestCandidate(api,directory,notes,plan,candidate.tag,candidate.commit,id,templates);
    return {candidate:signed,candidate_sha256:candidateFingerprint(signed),draft:state.draft,release_id:id};
  }finally{await rm(downloaded,{recursive:true,force:true});}
}
export async function attestCandidate(api: ReleaseAdapter,directory: string,notes: string,plan: string,tag: string,commit: string,releaseId: number,templates?:InstallerTemplates): Promise<Candidate> {
  numeric(releaseId);
  const selected=await selectCandidate({...api,createTag:async()=>{throw Error('release tag is missing');}},tag,commit);
  if(selected.release_id!==releaseId) throw Error('numeric release identity mismatch');
  const state=await api.release(releaseId);
  const body=await smallFile(notes);
  if(state.id!==releaseId || state.tag_name!==tag || state.name!==tag || state.prerelease || state.body!==body || typeof state.draft!=='boolean') throw Error('reviewed release identity or notes mismatch');
  const names=expectedAssets(tag);
  if(JSON.stringify([...state.assets].map(a=>a.name).sort())!==JSON.stringify(names)) throw Error('expected exactly eleven release assets');
  const local=(await readdir(directory)).sort();
  if(JSON.stringify(local)!==JSON.stringify(names)) throw Error('unexpected files in candidate asset directory');
  const ids=new Set<number>();const assets: Candidate['assets']=[];
  for(const name of names) {
    const asset=state.assets.find(a=>a.name===name)!;numeric(asset.id);
    if(ids.has(asset.id)) throw Error('duplicate asset ID');ids.add(asset.id);
    const bytes=await fileDigest(join(directory,name),limit(name));
    if(bytes.size!==asset.size) throw Error(`provider size mismatch: ${name}`);
    assets.push({id:asset.id,name,size:bytes.size,sha256:bytes.sha256});
  }
  const base=`https://github.com/${api.repository}/releases/download/${tag}`;
  if(await smallFile(join(directory,'SHA256SUMS'))!==await checksumLines(directory,tag.slice(1))) throw Error('archive checksum mismatch');
  if(await smallFile(join(directory,'tapid-release-v1.tsv'))!==await releaseRecord(directory,tag.slice(1),base)) throw Error('release record mismatch');
  const regenerated=await mkdtemp(join(tmpdir(),'tapid-installers-'));
  try {
    for(const name of names.filter(n=>n.endsWith('.tar.gz'))) await copyFile(join(directory,name),join(regenerated,name));
    await renderInstallers(regenerated,tag.slice(1),base,templates);
    for(const name of ['install.sh','install.ps1']) if(await smallFile(join(directory,name))!==await smallFile(join(regenerated,name))) throw Error('generated installer mismatch');
  } finally {await rm(regenerated,{recursive:true,force:true});}
  const candidate: Candidate={schema:'tapid-release-candidate-v1',repository:api.repository,tag,commit,tag_object:selected.tag_object,release_id:releaseId,title:tag,notes_sha256:digest(body),plan_sha256:digest(await smallFile(plan)),assets};
  return validateCandidate(candidate);
}
export async function publishCandidate(api: ReleaseAdapter,input: Candidate,notes: string,plan: string,templates?:InstallerTemplates): Promise<{release_id:number;tag:string;published:boolean}> {
  const candidate=validateCandidate(input);
  if(candidate.repository!==api.repository) throw Error('candidate repository mismatch');
  const directory=await mkdtemp(join(tmpdir(),'tapid-promotion-'));
  try {
    const state=await api.release(candidate.release_id);
    if(JSON.stringify(state.assets.map(a=>a.name).sort())!==JSON.stringify(expectedAssets(candidate.tag))) throw Error('release assets changed');
    for(const asset of candidate.assets) await api.download(asset,join(directory,asset.name));
    const actual=await attestCandidate(api,directory,notes,plan,candidate.tag,candidate.commit,candidate.release_id,templates);
    if(candidateFingerprint(candidate)!==candidateFingerprint(actual)) throw Error('release candidate changed after approval');
    if(!state.draft) {
      if(state.immutable!==true) throw Error('published release must be immutable');
      return {release_id:candidate.release_id,tag:candidate.tag,published:true};
    }
    const latest=await api.latest();
    if(latest && compare(latest.tag_name.slice(1),candidate.tag.slice(1))>0) throw Error('newer latest release exists; refusing rollback');
    await api.promote(candidate.release_id,!latest || compare(candidate.tag.slice(1),latest.tag_name.slice(1))>=0);
    const publicState=await api.release(candidate.release_id);
    if(publicState.draft || publicState.immutable!==true) throw Error('publication read-back is not public and immutable');
    const after=await attestCandidate(api,directory,notes,plan,candidate.tag,candidate.commit,candidate.release_id,templates);
    if(candidateFingerprint(candidate)!==candidateFingerprint(after)) throw Error('published candidate read-back changed');
    return {release_id:candidate.release_id,tag:candidate.tag,published:true};
  } finally {await rm(directory,{recursive:true,force:true});}
}


if(process.argv[1] && pathToFileURL(resolve(process.argv[1])).href===import.meta.url) {
  const {githubAdapter}=await import('./candidate_github.ts');
  const api=githubAdapter(process.env.GITHUB_REPOSITORY ?? 'LimeTip/tapid');
  const [command,...args]=process.argv.slice(2);
  let result: unknown;
  if(command==='select' && args.length===2) result=await selectCandidate(api,args[0],args[1]);
  else if(command==='intent' && args.length===1) result=validateReleaseIntent(JSON.parse(await smallFile(args[0])));
  else if(command==='download' && args.length===4) {
    await downloadCandidate(api,Number(args[0]),args[1],args[2],args[3]);result={release_id:Number(args[0]),downloaded:true};
  } else if(command==='unsigned' && args.length===6) {
    source(args[3],args[4]);
    const candidate=await attestUnsignedCandidate(args[0],args[1],args[2],api.repository,args[3],args[4],args[5],await templatesForCommit(args[4]));
    result={candidate,candidate_sha256:candidateFingerprint(candidate)};
  } else if(command==='validate-unsigned' && args.length===4) {
    const candidate=validateUnsignedCandidate(unwrapCandidate(JSON.parse(await smallFile(args[3]))));
    result={candidate_sha256:await validateUnsignedFiles(args[0],args[1],args[2],candidate,await templatesForCommit(candidate.commit))};
  } else if(command==='create-draft' && args.length===4) {
    const candidate=validateUnsignedCandidate(unwrapCandidate(JSON.parse(await smallFile(args[3]))));
    result=await createDraftCandidate(api,args[0],args[1],args[2],candidate,await templatesForCommit(candidate.commit));
  } else if(command==='attest' && args.length===6) {
    source(args[3],args[4]);
    const candidate=await attestCandidate(api,args[0],args[1],args[2],args[3],args[4],Number(args[5]),await templatesForCommit(args[4]));
    result={candidate,candidate_sha256:candidateFingerprint(candidate)};
  } else if(command==='publish' && args.length===3) {
    const candidate=validateCandidate(unwrapCandidate(JSON.parse(await smallFile(args[0]))));
    result=await publishCandidate(api,candidate,args[1],args[2],await templatesForCommit(candidate.commit));
  } else throw Error('usage: candidate.ts select TAG SHA | intent FILE | download RELEASE_ID DIR TAG SHA | unsigned DIR NOTES PLAN TAG SHA TAG_OBJECT | validate-unsigned DIR NOTES PLAN CANDIDATE | create-draft DIR NOTES PLAN UNSIGNED_CANDIDATE | attest DIR NOTES PLAN TAG SHA RELEASE_ID | publish CANDIDATE NOTES PLAN');
  process.stdout.write(JSON.stringify(result)+'\n');
}
