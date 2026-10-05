import { strict as assert } from 'node:assert';
import { test } from 'node:test';
import { githubAdapter } from './candidate_github.ts';
const commit='a'.repeat(40), object='b'.repeat(40);

test('GitHub adapter uses numeric publication and upload endpoints with minimal promotion payload',async()=>{
  const calls:{args:string[];payload:unknown}[]=[];
  const api=githubAdapter('LimeTip/tapid',async(args,payload)=>{calls.push({args,payload});return '{}';});
  await api.promote(123,true);await api.upload(123,'install.sh','/tmp/approved installer.sh');
  assert.equal(calls[0].args[1],'repos/LimeTip/tapid/releases/123');assert.deepEqual(calls[0].payload,{draft:false,make_latest:'true'});
  assert.equal(calls[1].args[1],'https://uploads.github.com/repos/LimeTip/tapid/releases/123/assets?name=install.sh');
  assert.equal(calls[1].args.at(-1),'/tmp/approved installer.sh');
});
test('GitHub adapter creates annotated tag with structured payload and preserves reviewed notes',async()=>{
  const calls:{args:string[];payload:unknown}[]=[];
  const api=githubAdapter('LimeTip/tapid',async(args,payload)=>{calls.push({args,payload});return JSON.stringify({sha:object});});
  await api.createTag('v0.0.12',commit);await api.createRelease('v0.0.12','notes\nwith actual newlines\n');
  assert.deepEqual(calls[0].payload,{tag:'v0.0.12',message:'Tapid v0.0.12',object:commit,type:'commit'});
  assert.deepEqual(calls[1].payload,{ref:'refs/tags/v0.0.12',sha:object});
  assert.equal((calls[2].payload as {body:string}).body,'notes\nwith actual newlines\n');
  assert.equal((calls[2].payload as {draft:boolean}).draft,true);
});
test('GitHub adapter treats only an explicit404 as missing and rejects lightweight or nested tags',async()=>{
  const missing=githubAdapter('LimeTip/tapid',async()=>{throw Error('gh: Not Found (HTTP 404)');});assert.equal(await missing.tag('v0.0.12'),null);
  const denied=githubAdapter('LimeTip/tapid',async()=>{throw Error('gh: Resource not accessible (HTTP 403)');});await assert.rejects(()=>denied.tag('v0.0.12'),/403/);
  const lightweight=githubAdapter('LimeTip/tapid',async()=> 'HTTP/2.0 200\n\n'+JSON.stringify({object:{type:'commit',sha:commit}}));await assert.rejects(()=>lightweight.tag('v0.0.12'),/annotated/);
  const nested=githubAdapter('LimeTip/tapid',async args=>args.includes('--include')?'HTTP/2.0 200\n\n'+JSON.stringify({object:{type:'tag',sha:object}}):JSON.stringify({object:{type:'tag',sha:object}}));await assert.rejects(()=>nested.tag('v0.0.12'),/directly/);
});
test('GitHub adapter restricts source to ancestor of main and selects all paginated matching releases',async()=>{
  const api=githubAdapter('LimeTip/tapid',async args=>args[1].includes('compare')?JSON.stringify({status:'ahead'}):JSON.stringify([[{id:1,tag_name:'v0.0.11'}],[{id:2,tag_name:'v0.0.12'}]]));
  assert.equal(await api.isOnMain(commit),true);assert.deepEqual(await api.releases('v0.0.12'),[{id:2,tag_name:'v0.0.12'}]);
  await assert.rejects(async()=>githubAdapter('attacker/repo/../../other'));
});
