import { strict as assert } from 'node:assert';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { fileURLToPath } from 'node:url';
import { test } from 'node:test';
import { mkdtemp, writeFile, rm, copyFile, readFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { checksumLines, releaseRecord } from './release.ts';
import { renderInstallers } from './bootstrap.ts';
import { candidateFingerprint, selectCandidate, attestCandidate, attestUnsignedCandidate, validateUnsignedCandidate, unwrapCandidate, createDraftCandidate, downloadCandidate, publishCandidate, validateReleaseIntent, type ReleaseAdapter, type ReleaseState } from './candidate.ts';

const commit = 'a'.repeat(40), object = 'b'.repeat(40);
const tag = 'v0.0.12', repository = 'LimeTip/tapid';
const intent = { schema: 'tapid-release-intent-v1', version: '0.0.12', baseline: 'v0.0.11', prepared_from:commit, notes: 'docs/releases/0.0.12.md', packages: [{name:'tapid',version:'0.0.12'}] };
const targets = ['aarch64-apple-darwin','aarch64-pc-windows-msvc','aarch64-unknown-linux-gnu','x86_64-apple-darwin','x86_64-pc-windows-msvc','x86_64-unknown-linux-gnu'];
function adapter(release: ReleaseState | null = null) {
  const patches: unknown[] = [];
  const api: ReleaseAdapter = {
    repository, tag: async () => ({object,commit}), isOnMain: async () => true,
    createTag: async () => {throw Error('must reuse tag');}, releases: async () => release ? [release] : [],
    release: async () => { if (!release) throw Error('no release'); return release; },
    latest: async () => null, download: async () => {throw Error('unexpected download');},
    promote: async (id, latest) => {patches.push({id,latest}); if (release) release = {...release,draft:false,immutable:true};},
    createRelease: async()=>{throw Error('unexpected creation');}, upload:async()=>{throw Error('unexpected upload');},
  };
  return {api,patches,setRelease(value: ReleaseState) {release=value;}};
}
async function fixture() {
  const directory = await mkdtemp(join(tmpdir(),'tapid-candidate-'));
  for (const target of targets) await writeFile(join(directory,`tapid-0.0.12-${target}.tar.gz`),target);
  await writeFile(join(directory,'SHA256SUMS'), await checksumLines(directory,'0.0.12'));
  await writeFile(join(directory,'tapid-release-v1.tsv'), await releaseRecord(directory,'0.0.12',`https://github.com/${repository}/releases/download/${tag}`));
  await writeFile(join(directory,'tapid-release-v1.tsv.sig'),'signed metadata checked by native verification');
  await renderInstallers(directory,'0.0.12',`https://github.com/${repository}/releases/download/${tag}`);
  const names = [...targets.map(t=>`tapid-0.0.12-${t}.tar.gz`),'SHA256SUMS','tapid-release-v1.tsv','tapid-release-v1.tsv.sig','install.sh','install.ps1'].sort();
  const assets = await Promise.all(names.map(async(name,index)=>({id:index+1,name,size:(await readFile(join(directory,name))).length})));
  const release: ReleaseState = {id:123,tag_name:tag,name:tag,body:'Reviewed notes\n',draft:true,prerelease:false,immutable:false,assets};
  const notes = join(directory,'../'+directory.split('/').pop()+'-notes.md');
  const plan = notes+'.json';
  await writeFile(notes,release.body); await writeFile(plan,JSON.stringify({packages:intent.packages}));
  return {directory,release,notes,plan,async cleanup(){await rm(directory,{recursive:true,force:true});await rm(notes,{force:true});await rm(plan,{force:true});}};
}

test('intent accepts only canonical reviewed release fields', () => {
  assert.deepEqual(validateReleaseIntent(intent),intent);
  for (const changed of [{...intent,notes:'../../notes.md'},{...intent,baseline:'v0.0.12'},{...intent,version:'01.2.3'},{...intent,packages:[...intent.packages,...intent.packages]},{...intent,extra:true}]) assert.throws(()=>validateReleaseIntent(changed));
});
test('unsigned approval binds ten files and rejects edits before draft creation',async()=>{
  const f=await fixture();try{
    const c=await attestUnsignedCandidate(f.directory,f.notes,f.plan,repository,tag,commit,object);
    assert.equal(c.assets.length,10);
    const mock=adapter(f.release);
    await writeFile(join(f.directory,'install.sh'),'tampered');
    await assert.rejects(()=>createDraftCandidate(mock.api,f.directory,f.notes,f.plan,c));
    assert.equal(mock.patches.length,0);
    assert.throws(()=>validateUnsignedCandidate({...c,extra:true}));
  }finally{await f.cleanup();}
});
test('existing complete draft reuses all original bytes and signatures',async()=>{
  const f=await fixture();try{
    const c=await attestUnsignedCandidate(f.directory,f.notes,f.plan,repository,tag,commit,object);
    const mock=adapter(f.release);mock.api.download=async(asset,path)=>{await copyFile(join(f.directory,asset.name),path);};
    const result=await createDraftCandidate(mock.api,f.directory,f.notes,f.plan,c);
    assert.equal(result.candidate.release_id,123);assert.equal(result.draft,true);
  }finally{await f.cleanup();}
});
test('historical installer attestation uses reviewed source templates after main changes',async()=>{
  const f=await fixture();try{
    const templates={
      'install.sh':'# reviewed historical installer\n'+await readFile(new URL('../../scripts/install.sh',import.meta.url),'utf8'),
      'install.ps1':'# reviewed historical installer\n'+await readFile(new URL('../../scripts/install.ps1',import.meta.url),'utf8'),
    };
    await renderInstallers(f.directory,'0.0.12',`https://github.com/${repository}/releases/download/${tag}`,templates);
    f.release.assets=await Promise.all(f.release.assets.map(async a=>({...a,size:(await readFile(join(f.directory,a.name))).length})));
    const mock=adapter(f.release);
    await assert.rejects(()=>attestUnsignedCandidate(f.directory,f.notes,f.plan,repository,tag,commit,object),/installer mismatch/);
    const approved=await attestUnsignedCandidate(f.directory,f.notes,f.plan,repository,tag,commit,object,templates);
    assert.equal(approved.assets.length,10);
    mock.api.download=async(a,p)=>copyFile(join(f.directory,a.name),p);
    const signed=await attestCandidate(mock.api,f.directory,f.notes,f.plan,tag,commit,123,templates);
    await publishCandidate(mock.api,signed,f.notes,f.plan,templates);assert.equal(mock.patches.length,1);
  }finally{await f.cleanup();}
});
test('download validates identity and refuses incomplete assets before fetching bytes',async()=>{
  const f=await fixture();try{
    const mock=adapter({...f.release,assets:f.release.assets.slice(1)});
    await assert.rejects(()=>downloadCandidate(mock.api,123,f.directory,tag,commit),/incomplete/);
  }finally{await f.cleanup();}
});
test('select reuses exact tag and refuses source or ancestry mismatch', async () => {
  const {api}=adapter(); assert.equal((await selectCandidate(api,tag,commit)).tag_object,object);
  await assert.rejects(()=>selectCandidate({...api,tag:async()=>({object,commit:'c'.repeat(40)})},tag,commit));
  await assert.rejects(()=>selectCandidate({...api,isOnMain:async()=>false},tag,commit));
  await assert.rejects(()=>selectCandidate({...api,releases:async()=>[{id:1,tag_name:tag},{id:2,tag_name:tag}] as ReleaseState[]},tag,commit));
});
test('attestation binds every asset byte, numeric ID, notes and publication plan', async () => {
  const f=await fixture(); try {
    const {api}=adapter(f.release); const c=await attestCandidate(api,f.directory,f.notes,f.plan,tag,commit,123);
    assert.equal(c.assets.length,11); assert.equal(candidateFingerprint(c),candidateFingerprint(JSON.parse(JSON.stringify(c))));
    const first=c.assets[0]; await writeFile(join(f.directory,first.name),'tampered');
    await assert.rejects(()=>attestCandidate(api,f.directory,f.notes,f.plan,tag,commit,123));
  } finally {await f.cleanup();}
});
test('draft approval is invalidated by asset ID or notes changes', async () => {
  const f=await fixture(); try {
    const mock=adapter(f.release);const c=await attestCandidate(mock.api,f.directory,f.notes,f.plan,tag,commit,123);
    mock.setRelease({...f.release,body:'unreviewed notes'});await assert.rejects(()=>attestCandidate(mock.api,f.directory,f.notes,f.plan,tag,commit,123));
    mock.setRelease({...f.release,assets:f.release.assets.map((a,i)=>i ? a : {...a,id:999})});
    const changed=await attestCandidate(mock.api,f.directory,f.notes,f.plan,tag,commit,123);
    assert.notEqual(candidateFingerprint(c),candidateFingerprint(changed));
  } finally {await f.cleanup();}
});
test('promotion downloads again, promotes once and resumes immutable public release',async()=>{
  const f=await fixture();try{
    const mock=adapter(f.release); mock.api.download=async(asset,path)=>{await copyFile(join(f.directory,asset.name),path);};
    const c=await attestCandidate(mock.api,f.directory,f.notes,f.plan,tag,commit,123);
    await publishCandidate(mock.api,c,f.notes,f.plan);assert.deepEqual(mock.patches,[{id:123,latest:true}]);
    await publishCandidate(mock.api,c,f.notes,f.plan);assert.equal(mock.patches.length,1);
  }finally{await f.cleanup();}
});
test('recovery verifies an older immutable public release without changing latest',async()=>{
  const f=await fixture();try{
    const mock=adapter({...f.release,draft:false,immutable:true});
    let latestReads=0,downloads=0;
    mock.api.latest=async()=>{latestReads++;return {...f.release,id:321,tag_name:'v0.0.13',draft:false,immutable:true};};
    mock.api.download=async(asset,path)=>{downloads++;await copyFile(join(f.directory,asset.name),path);};
    const approved=await attestCandidate(mock.api,f.directory,f.notes,f.plan,tag,commit,123);
    assert.deepEqual(await publishCandidate(mock.api,approved,f.notes,f.plan),{release_id:123,tag,published:true});
    assert.equal(downloads,11,'public recovery must still verify every asset');
    assert.equal(latestReads,0,'verified public recovery needs no latest-release lookup');
    assert.equal(mock.patches.length,0,'public recovery must not promote or change latest');
    mock.setRelease({...f.release,draft:false,immutable:false});
    await assert.rejects(()=>publishCandidate(mock.api,approved,f.notes,f.plan),/immutable/);
    mock.setRelease({...f.release,draft:false,immutable:true});
    await writeFile(f.plan,'unapproved plan');
    await assert.rejects(()=>publishCandidate(mock.api,approved,f.notes,f.plan),/changed/);
    assert.equal(mock.patches.length,0);
  }finally{await f.cleanup();}
});
test('promotion rejects tampering, newer latest release and nonimmutable public state',async()=>{
  const f=await fixture();try{
    const mock=adapter(f.release);const c=await attestCandidate(mock.api,f.directory,f.notes,f.plan,tag,commit,123);
    mock.api.download=async(asset,path)=>{await copyFile(join(f.directory,asset.name),path);};
    mock.api.latest=async()=>({...f.release,id:321,tag_name:'v0.0.13',draft:false});
    await assert.rejects(()=>publishCandidate(mock.api,c,f.notes,f.plan),/newer/);assert.equal(mock.patches.length,0);
    mock.api.latest=async()=>null;await writeFile(f.plan,'changed plan');await assert.rejects(()=>publishCandidate(mock.api,c,f.notes,f.plan));
  }finally{await f.cleanup();}
});

test('tag creation reads back the concrete annotated object exactly once',async()=>{
  const mock=adapter();let creates=0,present=false;
  mock.api.tag=async()=>present ? {object,commit} : null;
  mock.api.createTag=async(t,c)=>{assert.equal(t,tag);assert.equal(c,commit);creates++;present=true;};
  await selectCandidate(mock.api,tag,commit);await selectCandidate(mock.api,tag,commit);assert.equal(creates,1);
});
test('partial draft uploads only missing approved assets and never clobbers existing bytes',async()=>{
  const f=await fixture();try{
    const unsigned=await attestUnsignedCandidate(f.directory,f.notes,f.plan,repository,tag,commit,object);
    const mock=adapter({...f.release,assets:f.release.assets.slice(0,3)});let uploads=0;
    mock.api.download=async(a,p)=>copyFile(join(f.directory,a.name),p);
    let state={...f.release,assets:f.release.assets.slice(0,3)};
    mock.api.upload=async(id,name,path)=>{assert.equal(id,123);assert.equal(path,join(f.directory,name));uploads++;state={...state,assets:[...state.assets,f.release.assets.find(a=>a.name===name)!]};mock.setRelease(state);};
    const result=await createDraftCandidate(mock.api,f.directory,f.notes,f.plan,unsigned);
    assert.equal(uploads,8);assert.equal(result.candidate.assets.length,11);
  }finally{await f.cleanup();}
});
test('draft creation rejects changed original asset even when provider size matches',async()=>{
  const f=await fixture();try{
    const unsigned=await attestUnsignedCandidate(f.directory,f.notes,f.plan,repository,tag,commit,object);
    const mock=adapter({...f.release,assets:f.release.assets.slice(0,1)});
    mock.api.download=async(a,p)=>writeFile(p,Buffer.alloc(a.size,120));
    await assert.rejects(()=>createDraftCandidate(mock.api,f.directory,f.notes,f.plan,unsigned),/differs/);
  }finally{await f.cleanup();}
});
test('promotion refuses public mutable release and source tag object changes',async()=>{
  const f=await fixture();try{
    const mock=adapter(f.release);mock.api.download=async(a,p)=>copyFile(join(f.directory,a.name),p);
    const signed=await attestCandidate(mock.api,f.directory,f.notes,f.plan,tag,commit,123);
    mock.setRelease({...f.release,draft:false,immutable:false});await assert.rejects(()=>publishCandidate(mock.api,signed,f.notes,f.plan),/immutable/);
    mock.setRelease(f.release);mock.api.tag=async()=>({object:'c'.repeat(40),commit});await assert.rejects(()=>publishCandidate(mock.api,signed,f.notes,f.plan),/changed/);
    assert.equal(mock.patches.length,0);
  }finally{await f.cleanup();}
});

const execFileAsync=promisify(execFile);
test('CLI unsigned output roundtrips as verified envelope and refuses forged fingerprint',async()=>{
  const f=await fixture();const manifest=f.plan+'.candidate.json';try{
    const cli=fileURLToPath(new URL('./candidate.ts',import.meta.url));
    const actualCommit=(await execFileAsync('git',['rev-parse','HEAD'])).stdout.trim();
    const {stdout}=await execFileAsync(process.execPath,['--experimental-strip-types',cli,'unsigned',f.directory,f.notes,f.plan,tag,actualCommit,object],{env:{...process.env,GITHUB_REPOSITORY:repository}});
    const envelope=JSON.parse(stdout);await writeFile(manifest,stdout);
    assert.equal(unwrapCandidate(envelope).schema,'tapid-release-unsigned-candidate-v1');
    const checked=await execFileAsync(process.execPath,['--experimental-strip-types',cli,'validate-unsigned',f.directory,f.notes,f.plan,manifest],{env:{...process.env,GITHUB_REPOSITORY:repository}});
    assert.equal(JSON.parse(checked.stdout).candidate_sha256,envelope.candidate_sha256);
    envelope.candidate_sha256='0'.repeat(64);await writeFile(manifest,JSON.stringify(envelope));
    await assert.rejects(()=>execFileAsync(process.execPath,['--experimental-strip-types',cli,'validate-unsigned',f.directory,f.notes,f.plan,manifest],{env:{...process.env,GITHUB_REPOSITORY:repository}}));
  }finally{await f.cleanup();await rm(manifest,{force:true});}
});
