import { strict as assert } from 'node:assert';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { fileURLToPath } from 'node:url';
import { test } from 'node:test';
import { mkdtemp, writeFile, rm, copyFile, readFile, readdir } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { checksumLines, releaseRecord } from './release.ts';
import { renderInstallers, templatesForCommit, type InstallerTemplates } from './bootstrap.ts';
import { candidateFingerprint, selectCandidate, attestCandidate, attestUnsignedCandidate, validateUnsignedCandidate, validateCandidate, unwrapCandidate, createDraftCandidate, downloadCandidate, publishCandidate, validateReleaseIntent, type ReleaseAdapter, type ReleaseState } from './candidate.ts';

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
async function fixture(templates?:InstallerTemplates) {
  const directory = await mkdtemp(join(tmpdir(),'tapid-candidate-'));
  for (const target of targets) await writeFile(join(directory,`tapid-0.0.12-${target}.tar.gz`),target);
  await writeFile(join(directory,'SHA256SUMS'), await checksumLines(directory,'0.0.12'));
  await writeFile(join(directory,'tapid-release-v1.tsv'), await releaseRecord(directory,'0.0.12',`https://github.com/${repository}/releases/download/${tag}`));
  await writeFile(join(directory,'tapid-release-v1.tsv.sig'),'signed metadata checked by native verification');
  await renderInstallers(directory,'0.0.12',`https://github.com/${repository}/releases/download/${tag}`,templates);
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
    // The CLI attests committed templates; use those same inputs for this fixture.
    await renderInstallers(f.directory,'0.0.12',`https://github.com/${repository}/releases/download/${tag}`,await templatesForCommit(actualCommit));
    const {stdout}=await execFileAsync(process.execPath,['--experimental-strip-types',cli,'unsigned',f.directory,f.notes,f.plan,tag,actualCommit,object],{env:{...process.env,GITHUB_REPOSITORY:repository}});
    const envelope=JSON.parse(stdout);await writeFile(manifest,stdout);
    assert.equal(unwrapCandidate(envelope).schema,'tapid-release-unsigned-candidate-v1');
    const checked=await execFileAsync(process.execPath,['--experimental-strip-types',cli,'validate-unsigned',f.directory,f.notes,f.plan,manifest],{env:{...process.env,GITHUB_REPOSITORY:repository}});
    assert.equal(JSON.parse(checked.stdout).candidate_sha256,envelope.candidate_sha256);
    envelope.candidate_sha256='0'.repeat(64);await writeFile(manifest,JSON.stringify(envelope));
    await assert.rejects(()=>execFileAsync(process.execPath,['--experimental-strip-types',cli,'validate-unsigned',f.directory,f.notes,f.plan,manifest],{env:{...process.env,GITHUB_REPOSITORY:repository}}));
  }finally{await f.cleanup();await rm(manifest,{force:true});}
});

const stableTemplates:InstallerTemplates={
  'install.sh':'# shell\n@TAPID_BOOTSTRAP_VERSION@\n@TAPID_BOOTSTRAP_BASE_URL@\n# @TAPID_BOOTSTRAP_PINS@\n',
  'install.ps1':'# powershell\n@TAPID_BOOTSTRAP_VERSION@\n@TAPID_BOOTSTRAP_BASE_URL@\n# @TAPID_BOOTSTRAP_PINS@\n',
};
const baseUrl=`https://github.com/${repository}/releases/download/${tag}`;
async function attestFixture(f:Awaited<ReturnType<typeof fixture>>,signed:boolean,templates?:InstallerTemplates) {
  return signed
    ? attestCandidate(adapter(f.release).api,f.directory,f.notes,f.plan,tag,commit,123,templates)
    : attestUnsignedCandidate(f.directory,f.notes,f.plan,repository,tag,commit,object,templates);
}
test('stable candidate evidence preserves schemas, fingerprints and signature binding',async()=>{
  const f=await fixture(stableTemplates);try{
    const unsigned=await attestFixture(f,false,stableTemplates),signed=await attestFixture(f,true,stableTemplates);
    assert.equal(candidateFingerprint(unsigned),'8c6282c9210c970e0ce68d1ab54b4cac1d52835920dd7a9ee426d5de05ef3fe0');
    assert.equal(candidateFingerprint(signed),'6ba7ef2543eba7ae29e8d2c0f859d10e5fb85372139334aa17209d6583bd3517');
    assert.deepEqual(unsigned.assets,signed.assets.filter(a=>!a.name.endsWith('.sig')).map(({id,...a})=>a));
    assert.deepEqual({...signed,schema:unsigned.schema,assets:unsigned.assets,release_id:undefined},{...unsigned,release_id:undefined});
    assert.equal(unsigned.assets.length,10);assert.equal(signed.assets.length,11);
    assert.throws(()=>validateCandidate({...signed,release_id:0}),/invalid numeric/);
    assert.throws(()=>validateCandidate({...signed,assets:signed.assets.map(a=>({...a,id:0}))}),/invalid numeric/);
    assert.throws(()=>validateCandidate({...signed,assets:signed.assets.filter(a=>!a.name.endsWith('.sig'))}),/asset set/);
    assert.throws(()=>validateUnsignedCandidate({...unsigned,release_id:123}),/unexpected release fields/);
    assert.throws(()=>validateUnsignedCandidate({...unsigned,assets:unsigned.assets.map(a=>({...a,id:1}))}),/unexpected release fields/);
    assert.throws(()=>validateUnsignedCandidate({...unsigned,assets:[...unsigned.assets,{name:'tapid-release-v1.tsv.sig',size:1,sha256:'0'.repeat(64)}]}),/asset set/);
    const signature=join(f.directory,'tapid-release-v1.tsv.sig');
    const bytes=await readFile(signature);bytes[0]^=1;await writeFile(signature,bytes);
    assert.deepEqual(await attestFixture(f,false,stableTemplates),unsigned);
    const changed=await attestFixture(f,true,stableTemplates);
    assert.notEqual(candidateFingerprint(changed),candidateFingerprint(signed));
    assert.notDeepEqual(changed.assets.find(a=>a.name.endsWith('.sig')),signed.assets.find(a=>a.name.endsWith('.sig')));
  }finally{await f.cleanup();}
});
for(const signed of [false,true]) for(const mutation of ['archive','checksum','record','install.sh','install.ps1','archive-checksums','archive-record'] as const) {
  test(`${signed?'signed':'unsigned'} consistency rejects ${mutation}`,async()=>{
    const f=await fixture(stableTemplates);try{
      const name=mutation.startsWith('archive') ? `tapid-0.0.12-${targets[0]}.tar.gz` : mutation==='checksum' ? 'SHA256SUMS' : mutation==='record' ? 'tapid-release-v1.tsv' : mutation;
      const path=join(f.directory,name),bytes=await readFile(path);bytes[0]^=1;await writeFile(path,bytes);
      if(mutation==='archive-checksums'||mutation==='archive-record') await writeFile(join(f.directory,'SHA256SUMS'),await checksumLines(f.directory,'0.0.12'));
      if(mutation==='archive-record') await writeFile(join(f.directory,'tapid-release-v1.tsv'),await releaseRecord(f.directory,'0.0.12',baseUrl));
      // Regenerated metadata must not be masked by provider size checks.
      f.release.assets=await Promise.all(f.release.assets.map(async a=>({...a,size:(await readFile(join(f.directory,a.name))).length})));
      const expected=mutation==='archive'||mutation==='checksum' ? /archive checksum mismatch/ : mutation==='record'||mutation==='archive-checksums' ? /release record mismatch/ : /generated installer mismatch/;
      await assert.rejects(()=>attestFixture(f,signed,stableTemplates),expected);
    }finally{await f.cleanup();}
  });
}
for(const signed of [false,true]) for(const outcome of ['success','mismatch','render-failure']) {
  test(`${signed?'signed':'unsigned'} installer cleanup after ${outcome}`,async()=>{
    const f=await fixture(stableTemplates),root=await mkdtemp(join(tmpdir(),'tapid-cleanup-'));try{
      if(outcome==='mismatch') {
        const path=join(f.directory,'install.sh'),bytes=await readFile(path);bytes[0]^=1;await writeFile(path,bytes);
      }
      const templates=outcome==='render-failure' ? {...stableTemplates,'install.sh':'missing bootstrap markers'} : stableTemplates;
      const candidateUrl=new URL('./candidate.ts',import.meta.url).href;
      const args=JSON.stringify([f.directory,f.notes,f.plan,...(signed ? [tag,commit,123] : [repository,tag,commit,object]),templates]);
      const script=`
        import {strict as assert} from 'node:assert';
        import {attestCandidate,attestUnsignedCandidate} from ${JSON.stringify(candidateUrl)};
        const release=${JSON.stringify(f.release)},object=${JSON.stringify(object)},commit=${JSON.stringify(commit)},repository=${JSON.stringify(repository)};
        const api={repository,tag:async()=>({object,commit}),isOnMain:async()=>true,releases:async()=>[release],release:async()=>release};
        const run=()=>${signed ? 'attestCandidate(api,...' : 'attestUnsignedCandidate(...'}${args});
        ${outcome==='success' ? 'await run();' : 'await assert.rejects(run,/'+(outcome==='mismatch' ? 'generated installer mismatch' : 'installer must contain exactly one bootstrap marker')+'/);'}
      `;
      await execFileAsync(process.execPath,['--experimental-strip-types','--input-type=module','-e',script],{env:{...process.env,TMPDIR:root,TMP:root,TEMP:root}});
      assert.deepEqual(await readdir(root),[],'regeneration directory must be removed');
    }finally{await f.cleanup();await rm(root,{recursive:true,force:true});}
  });
}
