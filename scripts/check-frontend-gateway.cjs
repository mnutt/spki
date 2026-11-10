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
  const directory = fs.mkdtempSync(path.join(os.tmpdir(),'spki-gateway-'));
  const env = {...process.env,XDG_CONFIG_HOME:path.join(directory,'config')};
  const browser = await chromium.launch({headless:true,executablePath:process.env.CHROMIUM_PATH || '/usr/bin/chromium',args:['--host-resolver-rules=MAP *.local.sandstorm.io 127.0.0.1, MAP local.sandstorm.io 127.0.0.1','--no-proxy-server']});
  const context = await browser.newContext();
  const page = await context.newPage();page.setDefaultTimeout(60000);
  page.on('pageerror', error=>console.error('browser error:',redact(String(error))));
  let grainId;
  const label = `spki-gateway-${Date.now()}`;
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
    const projectDir=path.join(directory,'frontend');
    cli(['--profile','starter','init',projectDir]);
    cli(['--project',projectDir,'auth','--webkey-stdin','--allow-local-http'],developer);
    if (await page.locator('form.newApiToken').isVisible()) { await page.reload(); frame=await appFrame(); }
    assert(await frame.evaluate(async()=>{
      const {ByteStream}=await import('/__sandstorm/capnp/sandstorm/util.capnp.js');
      const {connectApplication}=await import('/__sandstorm/native-capnp/client.js');
      let app;
      try { app=await connectApplication(ByteStream); return false; }
      catch { return true; }
      finally { app?.transport.close(); }
    }),'a worker without connect() must reject application bootstrap');
    progress('checking browser assets and typed capnp-es RPC');
    fs.cpSync(path.join(root,'preview/fixtures/frontend'), projectDir, {recursive:true});
    const frontendConfig=path.join(projectDir,'sandstorm-pkgdef.capnp');
    let frontendText=fs.readFileSync(frontendConfig,'utf8').replace('profile = "another"','profile = "starter"').replace('src/worker.ts','worker.ts');
    frontendText=frontendText.replace('  compatibilityDate', '  ui = (entrypoints = ["ui/main.ts"]),\n  assets = ["public"],\n  schemas = ["counter.capnp"],\n  compatibilityDate');
    fs.writeFileSync(frontendConfig, frontendText);
    cli(['--project',projectDir,'dev','--once']);
    const workspaceProject=frontendText.match(/project = "([^"]+)"/)[1];
    frame=await require('./workspace-frame.cjs')(frame,workspaceProject);
    try { await frame.locator('output').filter({hasText:'0'}).waitFor({timeout:25000}); } catch(error) { console.log('frame:',await frame.locator('body').innerText()); console.log('handoff:', await frame.evaluate(()=>({hasHandoff:!!window.handoff, hasCounter:!!window.counter}))); throw error; }
    await frame.getByRole('button',{name:'Increment',exact:true}).click();
    await frame.locator('output').filter({hasText:'1'}).waitFor();
    assert.equal(await frame.locator('body').evaluate(el=>getComputedStyle(el).color),'rgb(20, 40, 60)');
    const rpcResult=await frame.evaluate(async()=>{
      const results=await Promise.all([counter.increment({amount:2}),counter.increment({amount:3})]);
      return (await counter.read()).value;
    });
    assert.equal(rpcResult,6);
    // Exercise the RPC bootstrap without a browser HTTP handoff. Existing handoffs
    // remain covered above and below, including cross-session rejection.
    assert.equal(await frame.evaluate(async()=>{
      const {BrowserCounter}=await import('/__sandstorm/capnp/counter.capnp.js');
      const {connectApplication}=await import('/__sandstorm/native-capnp/client.js');
      const originalFetch=window.fetch;
      window.fetch=()=>{throw new Error('bootstrap must not fetch');};
      try {
        window.application=await connectApplication(BrowserCounter);
        return (await application.read()).value;
      } finally { window.fetch=originalFetch; }
    }),6);
    assert.equal(await frame.evaluate(()=>fetch('/__spki_internal_connect').then(r=>r.status)),404);
    assert.equal((await request(developer,'/__spki_internal_connect')).status,404);
    assert.deepEqual(await frame.evaluate(async()=>{
      const {ByteStream}=await import('/__sandstorm/capnp/sandstorm/util.capnp.js');
      let bytes;
      const stream=new ByteStream.Server({write(params){bytes=Array.from(params.data);}}).client();
      await application.echo({stream});
      return bytes;
    }),[1,2,3]);
    assert(await frame.evaluate(async()=>{
      const {BrowserCounter}=await import('/__sandstorm/capnp/counter.capnp.js');
      const {connectBrowserNativeCapnp}=await import('/__sandstorm/native-capnp/client.js');
      const client=connectBrowserNativeCapnp(handoff,BrowserCounter);
      const pending=Promise.resolve(client.wait()).then(()=>false,()=>true);
      await new Promise(resolve=>setTimeout(resolve,100));
      client.transport.close();
      return await pending;
    }));
    console.log('PASS: browser callback capability, streaming call and cancellation');
    const descriptor = await frame.evaluate(async()=>{
      const {nativeCapnpPowerboxDescriptorInfo}=await import('/__sandstorm/native-capnp/client.js');
      const {BrowserCounter}=await import('/__sandstorm/capnp/counter.capnp.js');
      return await nativeCapnpPowerboxDescriptorInfo(BrowserCounter);
    });
    assert(descriptor.ok && descriptor.descriptor.length>0);
    const second=await page.context().newPage();
    try {
      await second.goto(page.url());
      let other;
      for(let i=0;i<100;i++) {
        other=second.frames().find(f=>f.url().includes('.local.sandstorm.io') && f!==second.mainFrame());
        if(other && await other.locator('body').count()) break;
        await second.waitForTimeout(100);
      }
      other=await require('./workspace-frame.cjs')(other,workspaceProject);
      await other.locator('output').filter({hasText:'6'}).waitFor({timeout:15000});
      const sessionA=await frame.evaluate(async()=>(await application.session()).id);
      const sessionB=await other.evaluate(async()=>{
        const {BrowserCounter}=await import('/__sandstorm/capnp/counter.capnp.js');
        const {connectApplication}=await import('/__sandstorm/native-capnp/client.js');
        const app=await connectApplication(BrowserCounter);
        try { return (await app.session()).id; } finally { app.transport.close(); }
      });
      assert(sessionA && sessionB && sessionA!==sessionB,'bootstrap did not use each browser session');
      const handoff=await frame.evaluate(()=>window.handoff);
      const denied=await other.evaluate(async(handoff)=>{
        const {BrowserCounter}=await import('/__sandstorm/capnp/counter.capnp.js');
        const {connectBrowserNativeCapnp}=await import('/__sandstorm/native-capnp/client.js');
        const foreign=connectBrowserNativeCapnp(handoff,BrowserCounter);
        try { await foreign.read(); return false; } catch { return true; } finally { foreign.transport.close(); }
      },handoff);
      assert(denied,'handoff crossed browser session boundary');
      assert.equal(await other.evaluate(async()=>(await counter.increment({amount:1})).value),7);
    } finally { await second.close(); }
    console.log('PASS: two browser sessions, cached worker export and session-bound handoff isolation');
    console.log('checking RPC after the host post-response deadline');
    await page.waitForTimeout(35000);
    assert.equal(await frame.evaluate(async()=>(await application.read()).value),7);
    const watcher=spawn(path.join(root,'target/debug/spki'),['--project',projectDir,'dev'],{env,stdio:['ignore','pipe','pipe']});
    let output=''; watcher.stdout.on('data',data=>output+=data);watcher.stderr.on('data',data=>output+=data);
    const revisions=()=>output.split('Live revision').length-1;
    async function waitRevision(previous) {
      for(let i=0;i<200;i++) { if(revisions()>previous) return; if(watcher.exitCode!==null) break; await page.waitForTimeout(100); }
      throw new Error(redact(output));
    }
    try {
      await waitRevision(0);
      await page.route('**/__spki/v1/revision',route=>route.abort());
      let before=revisions();
      fs.writeFileSync(path.join(projectDir,'ui/style.css'),'body {color: rgb(80, 60, 40)}');
      await waitRevision(before);
      await frame.waitForFunction(()=>counter.connection.closed && application.connection.closed);
      await page.unroute('**/__spki/v1/revision');
      await frame.waitForFunction(()=>getComputedStyle(document.body).color==='rgb(80, 60, 40)',{},{timeout:20000});
      await frame.locator('output').filter({hasText:'0'}).waitFor();
      before=revisions();
      fs.writeFileSync(path.join(projectDir,'public/extra.txt'),'new public asset');
      await waitRevision(before);
      assert.equal(await frame.evaluate(()=>fetch('/extra.txt').then(r=>r.text())),'new public asset');
      before=revisions();
      fs.unlinkSync(path.join(projectDir,'public/extra.txt'));
      await waitRevision(before);
      assert.equal(await frame.evaluate(()=>fetch('/extra.txt').then(r=>r.status)),404);
    } finally { watcher.kill('SIGTERM'); await new Promise(resolve=>{ if(watcher.exitCode!==null) resolve(); else watcher.once('exit',resolve); }); }
    console.log('PASS: CSS/public watching, deleted assets disappear, old RPC connections close on replacement');
    console.log('PASS: browser TypeScript, CSS, public index, generated schemas and pipelined typed browser RPC');
  } finally {
    try {
    if(grainId) {
      console.log('cleaning up test grain');
      await call('moveGrainsToTrash',[grainId]);
    }
    } finally {
      await browser.close();
      fs.rmSync(directory,{recursive:true,force:true});
    }
  }
})().catch(error=>{console.error(`FAIL (${stage}): ${redact(String(error))}`);process.exit(1)});
