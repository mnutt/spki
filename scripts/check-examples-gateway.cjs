#!/usr/bin/env node
// Run against the SPKI development mount. Creates and trashes only its own test grain.
// No SPK upload/install. PLAYWRIGHT_MODULE may point to an existing Playwright installation.
const {chromium} = require(process.env.PLAYWRIGHT_MODULE || process.argv[2] || 'playwright');
const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');
const http = require('node:http');
const {spawnSync,spawn} = require('node:child_process');
const assert = require('node:assert/strict');
const root = path.resolve(__dirname, '..');
const origin = 'http://local.sandstorm.io:6091';
const appId = 'vgsmnudpv4aup9cejg27p8cg029av8k3nr7h5yuduycjcz6hn49h';
let stage = 'starting browser';
function progress(value) { stage=value; console.log(value); }
const secrets = [];
function redact(message) { for (const secret of secrets) message = message.split(secret).join('[redacted]'); return message; }
function request(webkey, route) {
  const url = new URL(webkey); const token = url.hash.slice(1); url.hash=''; url.pathname=route;
  return new Promise((resolve,reject)=>{
    const req = http.get(url,{agent:false,headers:{Authorization:`Bearer ${token}`},lookup:(_hostname,options,callback)=>options.all?callback(null,[{address:'127.0.0.1',family:4}]):callback(null,'127.0.0.1',4)},res=>{
      let body='';res.on('data',chunk=>body+=chunk);res.on('end',()=>resolve({status:res.statusCode,body}));
    });req.on('error',reject);req.setTimeout(30000,()=>req.destroy(new Error('gateway timeout')));
  });
}
(async()=>{
  const directory = fs.mkdtempSync(path.join(os.tmpdir(),'spki-examples-'));
  const env = {...process.env,XDG_CONFIG_HOME:path.join(directory,'config'),SPKI_ESBUILD:path.join(root,'build/tools/esbuild')};
  const browser = await chromium.launch({headless:true,executablePath:process.env.CHROMIUM_PATH || '/usr/bin/chromium',args:['--host-resolver-rules=MAP *.local.sandstorm.io 127.0.0.1, MAP local.sandstorm.io 127.0.0.1','--no-proxy-server']});
  const context = await browser.newContext();
  const page = await context.newPage();page.setDefaultTimeout(60000);
  page.on('pageerror', error=>console.error('browser error:',redact(String(error))));
  let grainId;
  const label = `spki-examples-${Date.now()}`;
  const cli = (args,input,success=true)=>{
    const result=spawnSync(path.join(root,'target/debug/spki'),args,{env,input,encoding:'utf8',timeout:45000});
    assert.equal(result.status===0,success,redact(`${result.stdout}\n${result.stderr}`));
    for(const secret of secrets) assert(!(result.stdout+result.stderr).includes(secret),'CLI leaked a credential');
    return result;
  };
  const call = (name,...args)=>page.evaluate(({name,args})=>new Promise((resolve,reject)=>{setTimeout(()=>reject(new Error('Meteor method timeout')),30000);Meteor.call(name,...args,(error,value)=>error?reject(new Error(error.reason || error.message)):resolve(value));}),{name,args});
  try {
    progress('logging into dev account');
    await page.goto(origin);
    await page.getByText('with a Dev account',{exact:false}).click();
    await page.getByRole('button',{name:'Alice (admin)',exact:true}).click();
    await page.waitForFunction(()=>Boolean(Meteor.userId()));
    progress('creating development grain');
    await page.goto(`${origin}/apps/${appId}`);
    await page.locator('.app-details.dev-background').waitFor();
    await page.locator('button.action').filter({hasText:'Create new preview'}).click();
    await page.waitForURL(/\/grain\/[^/]+/);
    grainId = new URL(page.url()).pathname.split('/')[2];
    console.log('test grain:',grainId);
    process.once('SIGINT',async()=>{try {await call('moveGrainsToTrash',[grainId]);} finally {await browser.close();process.exit(130);}});
    await page.waitForFunction(()=>[...document.querySelectorAll('iframe')].some(f=>f.src.includes('.local.sandstorm.io')));
    async function appFrame() {
      await page.waitForFunction(()=>[...document.querySelectorAll('iframe')].some(f=>f.src.includes('.local.sandstorm.io')));
      for(let n=0;n<120;n++) {
        const frame=page.frames().find(f=>f.url().includes('.local.sandstorm.io') && f.url()!==page.url());
        if(frame && await frame.locator('body').count()) return frame;
        await page.waitForTimeout(250);
      }
      throw new Error('preview frame did not load');
    }
    let frame = await appFrame();
    await frame.waitForFunction(()=>document.body?.innerText.includes('count'));
    assert.equal(await frame.evaluate(()=>fetch('/increment').then(r=>r.json()).then(x=>x.count)),1);
    async function key(role,suffix) {
      if (!await page.locator('form.newApiToken').isVisible()) await page.getByTitle('Get webkey',{exact:true}).click();
      const roles=page.locator('form.newApiToken select');
      await roles.waitFor();
      const labels=(await roles.locator('option').allTextContents()).map(text=>text.trim());
      assert.deepEqual(labels,['all access','Preview','Developer']);
      await roles.selectOption({index:labels.indexOf(role)});
      await page.locator('form.newApiToken input[type=text]').fill(`${label}-${suffix}`);
      await page.locator('form.newApiToken button').click();
      console.log(`submitted ${role} key request`);
      const link=page.locator('a[href*="#"]').filter({hasText:/^http/});await link.waitFor();
      const webkey=await link.getAttribute('href');secrets.push(webkey,new URL(webkey).hash.slice(1));
      await page.getByRole('button',{name:'Go back',exact:true}).click();
      await page.locator('form.newApiToken').waitFor();
      console.log(`created ${role} webkey`);
      return webkey;
    }
    progress('minting deployment and preview-only webkeys through the UI');
    const developer=await key('Developer','developer');
    const viewer=await key('Preview','viewer');
    progress('checking actual gateway permissions');
    assert.equal((await request(viewer,'/__spki/v1/info')).status,403);
    assert.equal((await request(developer,'/__spki/v1/info')).status,200);
    frame=await appFrame();
    assert.equal(await frame.evaluate(()=>fetch('/__spki/v1/info').then(r=>r.status)),404);
    cli(['auth','--webkey-stdin','--allow-local-http'],developer);
    cli(['auth','status']);
    cli(['--profile','viewer','auth','--webkey-stdin','--allow-local-http'],viewer,false);

    if (await page.locator('form.newApiToken').isVisible()) { await page.reload(); frame=await appFrame(); }
    const names=['hello','storage-counter','app-skeleton','capnp-rpc','object-store','typescript','browser-capnp','counter','powerbox-http','capability-provider'];
    for(const name of names) {
      progress(`checking example ${name}`);
      const projectDir=path.join(directory,name);
      fs.cpSync(path.join(root,'examples',name),projectDir,{recursive:true,
        filter:p=>!p.split(path.sep).some(part=>['dist','node_modules','.spki'].includes(part))});
      if(name==='capability-provider') {
        const worker=path.join(projectDir,'worker.js');
        fs.writeFileSync(worker,fs.readFileSync(worker,'utf8').replace('    try {', `    try {
          if (url.pathname === '/test-save') {
            const cap = await api.webSession({pathPrefix:'/shared'});
            try { await api.storage().put('test-route-token', await cap.save({label:'Restart test'})); }
            finally { await cap.drop(); }
            return Response.json({ok:true});
          }
          if (url.pathname === '/test-restore') {
            return api.use(await api.storage().get('test-route-token'), cap => cap.fetch('/info'));
          }
          if (url.pathname === '/test-revoke') {
            const token = await api.storage().get('test-route-token');
            await api.revoke(token);
            try { await api.use(token, cap => cap.fetch('/info')); }
            catch { return Response.json({rejected:true}); }
            return Response.json({rejected:false});
          }
          if (url.pathname === '/test-ephemeral') {
            const cap = await api.webSession({pathPrefix:'/shared',persistent:false});
            try { await cap.save(); return Response.json({rejected:false}); }
            catch { return Response.json({rejected:true}); }
            finally { await cap.drop(); }
          }
        `));
      }
      cli(['--project',projectDir,'auth','--webkey-stdin','--allow-local-http'],developer);
      cli(['--project',projectDir,'dev','--once']);
      await frame.goto(new URL('/',frame.url()).href);
      async function json(route,post=false) {
        return frame.evaluate(async({route,post})=>{
          const response=await fetch(route,{method:post?'POST':'GET'});
          if(!response.ok) throw new Error(await response.text());
          return response.json();
        },{route,post});
      }
      if(name==='capability-provider') {
        for(const kind of ['web-session','api-session']) {
          const result=await json('/api/export-'+kind,true);
          assert(result.ok && result.saved);
          assert.equal(result.directCall.status,200);
          assert.equal(result.restoredCall.status,200);
          assert.equal(result.directCall.body.path,kind==='web-session'?'/shared/info':'/api/v1/status');
          assert.equal(result.restoredCall.body.search,'?source=restored');
          assert(result.dropSaved.ok || result.dropSaved.unsupported);
          if(kind==='web-session') {
            assert.equal(result.directCall.body.forwardedHeader,'from-claimed-fetch');
            assert(!result.directCall.body.session.sessionId);
          } else assert.equal(result.directCall.body.sessionType,'api');
        }
        assert((await json('/test-ephemeral')).rejected);
        assert((await json('/test-save')).ok);
        await call('shutdownGrain',grainId);
        let ready=false;
        for(let attempt=0;attempt<40;attempt++) {
          try { ready=(await request(developer,'/__spki/v1/info')).status===200; } catch {}
          if(ready) break;
          await page.waitForTimeout(250);
        }
        assert(ready,'Preview did not recover');
        await page.reload(); frame=await appFrame();
        assert.equal((await json('/test-restore')).path,'/shared/info');
        const worker=path.join(projectDir,'worker.js');
        fs.writeFileSync(worker,fs.readFileSync(worker,'utf8').replace('provider-web-session','provider-web-session-v2'));
        cli(['--project',projectDir,'dev','--once']);
        assert.equal(await frame.evaluate(()=>fetch('/test-restore').then(r=>r.headers.get('etag'))),'"provider-web-session-v2"');
        const drop=await frame.evaluate(async()=>{const r=await fetch('/test-revoke');return {status:r.status,body:await r.json()};});
        if(drop.status===500) {
          assert(drop.body.message.includes('Supervisor can only drop wakelocks for now'));
          assert.equal((await json('/test-restore')).path,'/shared/info');
          console.log('CONFIRMED: legacy supervisor drop limitation; failed drop leaves token usable');
        } else assert(drop.body.rejected);
        console.log('PASS: route grant survives restart/replacement; ephemeral save is rejected');
      } else if(name==='hello') {
        const data=await json('/hello?test=1');
        assert.equal(data.pathname,'/hello');assert(data.hasStorageBinding && data.hasNativeRpcBinding);
      } else if(name==='storage-counter') {
        const before=Number(await frame.locator('.count').innerText());
        await frame.goto(frame.url());assert.equal(Number(await frame.locator('.count').innerText()),before+1);
        await call('shutdownGrain',grainId);
        let ready=false;
        for(let attempt=0;attempt<40;attempt++) {
          try { ready=(await request(developer,'/__spki/v1/info')).status===200; } catch {}
          if(ready) break;
          await page.waitForTimeout(250);
        }
        assert(ready,'Preview API did not recover after restart');
        await page.reload(); frame=await appFrame();
        assert(Number(await frame.locator('.count').innerText())>before,'counter did not survive restart');
      } else if(name==='app-skeleton') {
        assert.equal((await json('/hello?name=SPKI')).greeting,'Hello, SPKI!');
        assert.equal((await json('/increment',true)).value,1);
        assert.equal((await json('/increment',true)).value,2);
        assert(Array.isArray((await json('/session')).permissions));
      } else if(name==='capnp-rpc') {
        const data=await json('/?name=SPKI');
        assert.equal(data.result.message,'Hello, SPKI');
        assert.equal(data.casualGreetingResult.message,'Hi, SPKI');
        assert.equal(await frame.evaluate(async()=>{
          const {Greeter}=await import('/__sandstorm/capnp/greeter.capnp.js');
          const {Greeting}=await import('/__sandstorm/capnp/greeting.capnp.js');
          const {connectApplication}=await import('/__sandstorm/native-capnp/client.js');
          const greeter=await connectApplication(Greeter);
          try {
            const callback=new Greeting.Server({read:()=>({message:'browser callback'})}).client();
            return (await greeter.useGreeting({greeting:callback})).message;
          } finally { greeter.transport.close(); }
        }),'browser callback');
      } else if(name==='object-store') {
        const data=await json('/');assert.equal(data.object.body,'cover image placeholder\n');
        assert.equal(data.listing.objects.length,2);
        assert.equal(await frame.evaluate(async()=>{
          const {ObjectStore}=await import('/__sandstorm/capnp/object-store.capnp.js');
          const {connectApplication}=await import('/__sandstorm/native-capnp/client.js');
          const store=await connectApplication(ObjectStore);
          try {
            const object=await store.openObject({bucket:'docs',key:'readme.txt'});
            const response=await object.object.get({path:'',context:{},ignoreBody:false});
            return new TextDecoder().decode(response.content.body.bytes.toUint8Array());
          } finally { store.transport.close(); }
        }),'object-store example\n');
      } else if(name==='typescript') {
        assert.equal((await json('/increment?step=3',true)).value,3);
        assert.equal(await frame.evaluate(async()=>{
          const {TypedCounter}=await import('/__sandstorm/capnp/typed-counter.capnp.js');
          const {connectApplication}=await import('/__sandstorm/native-capnp/client.js');
          const counter=await connectApplication(TypedCounter);
          try { return (await counter.increment({step:2})).value; }
          finally { counter.transport.close(); }
        }),5);
      } else if(name==='browser-capnp' || name==='counter') {
        await frame.locator('#value').filter({hasText:/^0$/}).waitFor();
        await frame.getByRole('button',{name:'Increment',exact:true}).click();
        await frame.locator('#value').filter({hasText:/^1$/}).waitFor();
      } else {
        await frame.waitForFunction(()=>!document.querySelector('button').disabled);
      }
      console.log(`PASS: ${name}`);
    }
  } finally {
    try { if(grainId) { console.log('cleaning up test grain'); await call('moveGrainsToTrash',[grainId]); } }
    finally { await browser.close(); fs.rmSync(directory,{recursive:true,force:true}); }
  }
})().catch(error=>{console.error(`FAIL (${stage}): ${redact(String(error))}`);process.exit(1)});
