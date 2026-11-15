// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Official Node/browser 11.8.0 (c6492ef78c7bfb97931e43075763ce84ee8fda7e),
// Express 5.1.0, Playwright 1.64.1. Synthetic framework traffic only.
import assert from 'node:assert/strict';
import {createRequire} from 'node:module';
import {pathToFileURL} from 'node:url';
import {spawn} from 'node:child_process';
import {createInterface} from 'node:readline';
import {mkdtemp,writeFile,rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join} from 'node:path';

const [entry,browserEntry,run,mode] = process.argv.slice(2);
if(mode==='uncaught' || mode==='rejection') {
 const S=await import(pathToFileURL(entry).href);assert.equal(S.SDK_VERSION,'11.8.0');
 S.init({dsn:process.env.SENTRY_DSN,release:run+'.'+mode,defaultIntegrations:false,sendClientReports:false,
  integrations:[S.onUncaughtExceptionIntegration(),S.onUnhandledRejectionIntegration({mode:'strict'})],
  beforeSend(event){
   event.server_name='synthetic.example';event.tags={'synthetic.run':run+'.'+mode};
   for(const exception of event.exception?.values??[]) for(const frame of exception.stacktrace?.frames??[]) {
    frame.filename='synthetic:///framework.mjs';frame.abs_path='synthetic:///framework.mjs';
   }
   console.log(JSON.stringify({id:event.event_id}));return event;
  },
 });
 if(mode==='uncaught') setTimeout(()=>{throw new Error('Synthetic uncaught failure');},0);
 else Promise.reject(new Error('Synthetic rejected promise'));
} else if(mode==='server') {
 const S=await import(pathToFileURL(entry).href);
 assert.equal(S.SDK_VERSION,'11.8.0');
 const events=[];let spanCount=0;
 S.init({dsn:process.env.SENTRY_DSN,release:run,environment:'synthetic-framework',
  enableLogs:true,tracesSampleRate:1,sendClientReports:false,sendDefaultPii:false,
  beforeSendSpan(span){spanCount++;return span;},
  beforeSend(event){
   event.server_name='synthetic.example';
   for(const exception of event.exception?.values??[]) for(const frame of exception.stacktrace?.frames??[]) {
    frame.filename='synthetic:///framework.mjs';frame.abs_path='synthetic:///framework.mjs';
   }
   events.push({id:event.event_id,trace:event.contexts?.trace?.trace_id});return event;
  },
 });
 const require=createRequire(pathToFileURL(entry));
 const express=require('express');assert.equal(require('express/package.json').version,'5.1.0');
 const downstream=express();
 downstream.get('/work',(_req,res)=>{
  S.logger.info(S.logger.fmt`Synthetic framework item ${7}`,{'synthetic.run':run});
  S.metrics.count('synthetic.framework.requests',2,{attributes:{'synthetic.run':run}});
  for(const value of [1,3,5,7,9]) S.metrics.distribution('synthetic.framework.duration',value,{unit:'millisecond',attributes:{'synthetic.run':run}});
  S.metrics.gauge('synthetic.framework.queue',7,{attributes:{'synthetic.run':run}});
  res.json({ok:true});
 });
 const downstreamServer=await new Promise(resolve=>{const server=downstream.listen(0,'127.0.0.1',()=>resolve(server));});
 const app=express();
 app.get('/',(_req,res)=>res.type('html').send('<!doctype html><title>Synthetic framework fixture</title>'));
 app.get('/checkout',async(_req,_res)=>{
  S.setUser({id:'synthetic-framework-user',username:'synthetic-user'});
  S.setTag('synthetic.run',run);
  S.setContext('synthetic_context',{nested:{number:7,flag:true,missing:null},unicode:'Synthetic café 漢字'});
  S.addBreadcrumb({category:'synthetic',message:'Synthetic checkout',data:{count:7,flag:true,nested:{empty:null}}});
  const response=await fetch(`http://127.0.0.1:${downstreamServer.address().port}/work`);assert.equal(response.status,200);
  throw new Error('Synthetic framework outer',{cause:new TypeError('Synthetic framework cause')});
 });
 S.setupExpressErrorHandler(app);
 app.use((_error,_req,res,_next)=>res.status(500).json({synthetic:true}));
 const server=await new Promise(resolve=>{const value=app.listen(0,'127.0.0.1',()=>resolve(value));});
 console.log(JSON.stringify({port:server.address().port}));
 process.stdin.once('data',async()=>{
  server.close();downstreamServer.close();
  assert.ok(await S.flush(10000));await S.close(10000);
  console.log(JSON.stringify({events,span_count:spanCount}));
 });
} else {
 assert.ok(typeof Bun!=='undefined','Run the browser driver using Bun');
 const require=createRequire(pathToFileURL(entry));
 const {chromium}=await import(pathToFileURL(require.resolve('playwright')).href);
 const directory=await mkdtemp(join(tmpdir(),'temps-synthetic-framework-'));
 const child=spawn('node',[process.argv[1],entry,browserEntry,run,'server'],{env:process.env,stdio:['pipe','pipe','pipe']});
 // SDK stderr may contain credentials on failure; never forward it.
 child.stderr.resume();
 const lines=createInterface({input:child.stdout});const iterator=lines[Symbol.asyncIterator]();
 const next=async()=>{let timer;try{const result=await Promise.race([iterator.next(),new Promise((_,reject)=>{timer=setTimeout(()=>reject(new Error('Synthetic SDK timeout')),30000);})]);assert.ok(!result.done);return JSON.parse(result.value);}finally{clearTimeout(timer);}};
 const exited=new Promise(resolve=>child.once('exit',resolve));
 let browser;
 try {
  const {port}=await next();
  const wrapper=join(directory,'entry.mjs');
  await writeFile(wrapper,`// SPDX-FileCopyrightText: 2024-2026 Temps Contributors\n// SPDX-License-Identifier: MIT OR Apache-2.0\nimport * as SDK from ${JSON.stringify(browserEntry)}; window.SyntheticSentry=SDK;`);
  const build=await Bun.build({entrypoints:[wrapper],target:'browser',format:'iife'});assert.ok(build.success);
  browser=await chromium.launch({headless:true});const page=await browser.newPage();
  await page.goto(`http://127.0.0.1:${port}/`);await page.addScriptTag({content:await build.outputs[0].text()});
  const trace=await page.evaluate(async({dsn,release})=>{
   const S=window.SyntheticSentry;let spanCount=0;
   S.init({dsn,release,defaultIntegrations:false,sendClientReports:false,tracesSampleRate:1,dataCollection:{userInfo:false},beforeSendSpan(span){spanCount++;return span;}});
   let trace;
   await S.startSpan({name:'Synthetic browser checkout',op:'ui.action'},async span=>{
    trace=span.spanContext().traceId;
    const response=await fetch('/checkout',{headers:S.getTraceData()});
    if(response.status!==500) throw new Error('Expected synthetic application failure');
   });
   if(!await S.flush(10000)) throw new Error('Synthetic browser flush failed');await S.close(10000);return {trace,span_count:spanCount};
  },{dsn:process.env.SENTRY_DSN,release:run});
  child.stdin.end('stop');const server=await next();assert.equal(await exited,0);
  assert.equal(server.events.length,1);assert.equal(server.events[0].trace,trace.trace);
  console.log(JSON.stringify({sdk:'11.8.0',express:'5.1.0',trace_id:trace.trace,event_id:server.events[0].id,release:run,span_count:server.span_count+trace.span_count}));
 } finally {
  child.kill();if(browser)await browser.close();await rm(directory,{recursive:true,force:true});
 }
}
