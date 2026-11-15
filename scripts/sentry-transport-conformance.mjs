// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Official Node 11.8.0 (c6492ef78c7bfb97931e43075763ce84ee8fda7e).
// A finite synthetic relay supplies Retry-After, then forwards the original
// compressed SDK bytes into Temps. No payload or credentials are printed.
import assert from 'node:assert/strict';
import http from 'node:http';
import {pathToFileURL} from 'node:url';
const [entry,run]=process.argv.slice(2);
const S=await import(pathToFileURL(entry).href);assert.equal(S.SDK_VERSION,'11.8.0');
const dsn=new URL(process.env.SENTRY_DSN);
assert.ok(['localhost','127.0.0.1','[::1]'].includes(dsn.hostname));
const endpoint=new URL(`/api/${dsn.pathname.slice(1)}/envelope/`,dsn.origin);
endpoint.searchParams.set('sentry_key',dsn.username);
let requests=0;let gzip=false;
const relay=http.createServer(async(req,res)=>{
 try {
  const chunks=[];let size=0;
  for await(const chunk of req){size+=chunk.length;assert.ok(size<=1024*1024);chunks.push(chunk);}
  gzip ||= req.headers['content-encoding']==='gzip';
  if(++requests===1){res.writeHead(429,{'Retry-After':'1'});res.end();return;}
  const headers={'Content-Type':'application/x-sentry-envelope'};
  if(req.headers['content-encoding'])headers['Content-Encoding']=req.headers['content-encoding'];
  const response=await fetch(endpoint,{method:'POST',headers,body:Buffer.concat(chunks)});
  assert.equal(response.status,200);res.writeHead(response.status);res.end();
 } catch {res.writeHead(500);res.end();}
});
await new Promise(resolve=>relay.listen(0,'127.0.0.1',resolve));
try {
 S.init({dsn:process.env.SENTRY_DSN,tunnel:`http://127.0.0.1:${relay.address().port}/relay`,defaultIntegrations:false,sendClientReports:false});
 S.setTag('synthetic.run',run);S.setExtra('synthetic.padding','x'.repeat(48*1024));
 const first=S.captureMessage('Synthetic rate limit first');assert.ok(await S.flush(10000));assert.equal(requests,1);
 const suppressed=S.captureMessage('Synthetic rate limit suppressed');assert.ok(await S.flush(10000));assert.equal(requests,1);
 await new Promise(resolve=>setTimeout(resolve,1100));
 const recovered=S.captureMessage('Synthetic rate limit recovered');assert.ok(await S.flush(10000));assert.equal(requests,2);
 await S.close(10000);assert.ok(gzip,'Official SDK must exercise compressed transport');
 console.log(JSON.stringify({sdk:S.SDK_VERSION,requests,retry_after_observed:true,gzip:true,first,suppressed,recovered}));
} finally {relay.close();relay.closeAllConnections();}
