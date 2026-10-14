import { execFile, spawn } from 'node:child_process';
import { createWriteStream } from 'node:fs';
import { Transform } from 'node:stream';
import { pipeline } from 'node:stream/promises';
import type { Asset, ReleaseAdapter, ReleaseState } from './candidate.ts';

function gh(args: string[], payload?: unknown): Promise<string> {
  return new Promise((resolve,reject)=>{
    const child=execFile('gh',args,{encoding:'utf8',maxBuffer:16*1024*1024},(error,stdout,stderr)=>{
      if(error) reject(Error(`GitHub API request failed: ${stderr.trim() || error.message}`)); else resolve(stdout);
    });
    if(payload!==undefined) child.stdin!.end(JSON.stringify(payload));
  });
}
export function githubAdapter(repository: string,runCommand:typeof gh=gh): ReleaseAdapter {
  if(!/^[A-Za-z0-9][A-Za-z0-9_-]*\/[A-Za-z0-9][A-Za-z0-9._-]*$/.test(repository)) throw Error('invalid repository');
  const endpoint=`repos/${repository}`;
  const request=async(path: string,method='GET',payload?:unknown)=>JSON.parse(await runCommand(['api',`${endpoint}/${path}`,'--method',method,...(payload===undefined?[]:['--input','-'])],payload));
  const optional=async(path:string)=>{
    // Use status-bearing API output. Other failures, including authentication and rate limits, must not look absent.
    const output=await runCommand(['api',`${endpoint}/${path}`,'--include']).catch(async(error:Error)=>{
      if(!error.message.includes('(HTTP 404)')) throw error;
      return null;
    });
    if(output===null) return null;
    const split=output.indexOf('\r\n\r\n')>=0 ? output.indexOf('\r\n\r\n')+4 : output.indexOf('\n\n')+2;
    if(split<2) throw Error('invalid GitHub API status response');
    return JSON.parse(output.slice(split));
  };
  return {
    repository,
    async tag(tag) {
      const ref=await optional(`git/ref/tags/${tag}`);
      if(!ref) return null;
      if(ref.object?.type!=='tag') throw Error('release tag must be annotated');
      const object=await request(`git/tags/${ref.object.sha}`);
      if(object.object?.type!=='commit') throw Error('release tag must point directly to a commit');
      return {object:ref.object.sha,commit:object.object.sha};
    },
    async isOnMain(commit) { const result=await request(`compare/${commit}...main`);return result.status==='ahead' || result.status==='identical'; },
    async createTag(tag,commit) {
      const object=await request('git/tags','POST',{tag,message:`Tapid ${tag}`,object:commit,type:'commit'});
      await request('git/refs','POST',{ref:`refs/tags/${tag}`,sha:object.sha});
    },
    async releases(tag) {
      const pages=JSON.parse(await runCommand(['api',`${endpoint}/releases?per_page=100`,'--paginate','--slurp']));
      return pages.flat().filter((release:ReleaseState)=>release.tag_name===tag);
    },
    async release(id) {return request(`releases/${id}`);},
    async latest() {return optional('releases/latest');},
    async createRelease(tag,notes) {return request('releases','POST',{tag_name:tag,target_commitish:'main',name:tag,body:notes,draft:true,prerelease:false,make_latest:'false'});},
    async upload(id,name,path) {
      if(!Number.isSafeInteger(id)||id<=0||!/^[-A-Za-z0-9._]+$/.test(name)) throw Error('invalid upload identity');
      await runCommand(['api',`https://uploads.github.com/${endpoint}/releases/${id}/assets?name=${encodeURIComponent(name)}`,'--method','POST','-H','Content-Type: application/octet-stream','--input',path]);
    },
    async download(asset: Asset,path: string) {
      if(!Number.isSafeInteger(asset.id)||asset.id<=0||!Number.isSafeInteger(asset.size)||asset.size<=0||asset.size>512*1024*1024) throw Error('invalid download asset');
      const child=spawn('gh',['api',`${endpoint}/releases/assets/${asset.id}`,'-H','Accept: application/octet-stream'],{stdio:['ignore','pipe','pipe']});
      let bytes=0,stderr='';child.stderr.on('data',chunk=>{stderr=(stderr+chunk.toString()).slice(-4096);});
      const complete=new Promise<void>((resolve,reject)=>{
        child.on('error',reject);child.on('close',code=>code===0?resolve():reject(Error(`asset download failed: ${stderr}`)));
      });
      const bounded=new Transform({transform(chunk,_encoding,callback){bytes+=chunk.length;callback(bytes>asset.size?Error('download exceeds provider size'):null,chunk);}});
      try {await Promise.all([complete,pipeline(child.stdout,bounded,createWriteStream(path,{flags:'wx'}))]);}
      catch(error){child.kill();throw error;}
      if(bytes!==asset.size) throw Error('download size does not match provider');
    },
    async promote(id,latest) {await request(`releases/${id}`,'PATCH',{draft:false,make_latest:latest?'true':'false'});},
  };
}
