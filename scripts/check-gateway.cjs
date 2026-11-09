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
  const env = {...process.env,XDG_CONFIG_HOME:path.join(directory,'config'),SPKI_ESBUILD:path.join(root,'build/tools/esbuild')};
  const browser = await chromium.launch({headless:true,executablePath:process.env.CHROMIUM_PATH || '/usr/bin/chromium',args:['--host-resolver-rules=MAP *.local.sandstorm.io 127.0.0.1, MAP local.sandstorm.io 127.0.0.1','--no-proxy-server']});
  const page = await browser.newPage();page.setDefaultTimeout(60000);
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
    progress('deploying TypeScript through the real gateway');
    const entry=path.join(directory,'worker.ts');
    const original=fs.readFileSync(path.join(root,'preview/fixtures/sdk-counter.js'),'utf8');
    fs.writeFileSync(entry,original+"\nexport const marker: string = 'gateway-test';\n");
    cli(['dev','--entry',entry,'--once']);
    await page.reload();frame=await appFrame();
    await frame.waitForFunction(()=>document.body?.innerText.includes('count'));
    assert.equal(await frame.evaluate(()=>fetch('/').then(r=>r.json()).then(x=>x.count)),1);
    progress('restarting grain and checking persistence');
    const before=JSON.parse((await request(developer,'/__spki/v1/status')).body).revision;
    await call('shutdownGrain',grainId);
    let restarted;
    for (let attempt=0;attempt<40;attempt++) {
      try {
        const response=await request(developer,'/__spki/v1/status');
        if(response.status===200) { restarted=JSON.parse(response.body).revision; break; }
      } catch {} // The gateway can still hold the old session immediately after shutdown.
      await page.waitForTimeout(250);
    }
    assert.equal(restarted,before,'revision must survive restart');
    await page.reload();frame=await appFrame();
    await frame.waitForFunction(()=>document.body?.innerText.includes('count'));
    assert.equal(await frame.evaluate(()=>fetch('/').then(r=>r.json()).then(x=>x.count)),1);
    progress('checking automatic browser reload');
    const htmlSource=fs.readFileSync(path.join(root,'preview/fixtures/reload.ts'),'utf8');
    fs.writeFileSync(entry,htmlSource);
    cli(['dev','--entry',entry,'--once']);
    // Navigate once from the JSON fixture to an HTML page. Further updates must reload themselves.
    const documentUrl=new URL('/nested/preview?test=1#position',frame.url()).href;
    await frame.goto(documentUrl);
    await frame.locator('#version').filter({hasText:'first'}).waitFor();
    assert.equal(await frame.locator('#counter').innerText(),'1');
    assert.equal(await frame.evaluate(()=>document.compatMode),'CSS1Compat');
    const readOnly=await frame.evaluate(()=>fetch('/__spki/v1/revision').then(r=>r.json()));
    assert.deepEqual(Object.keys(readOnly),['revision']);
    assert.equal(await frame.evaluate(()=>fetch('/__spki/v1/revision',{method:'POST'}).then(r=>r.status)),404);
    assert.equal((await request(developer,'/__spki/v1/revision')).status,404);
    const markup=await frame.content();
    for(const secret of secrets) assert(!markup.includes(secret),'credential leaked to preview HTML');
    await frame.evaluate(()=>window.reloadSentinel='unchanged');
    // Neither invalid code nor retrying the same revision should reload the document.
    fs.writeFileSync(entry,'export default { broken(');
    cli(['dev','--entry',entry,'--once'],undefined,false);
    fs.writeFileSync(entry,htmlSource);
    cli(['dev','--entry',entry,'--once']);
    await page.waitForTimeout(2500);
    assert.equal(await frame.evaluate(()=>window.reloadSentinel),'unchanged');
    // A network interruption must preserve the page; recovery must discover the new revision.
    await page.route('**/__spki/v1/revision',route=>route.abort());
    fs.writeFileSync(entry,htmlSource.replace("'first'","'second'"));
    cli(['dev','--entry',entry,'--once']);
    await page.waitForTimeout(2500);
    assert.equal(await frame.evaluate(()=>window.reloadSentinel),'unchanged');
    await page.unroute('**/__spki/v1/revision');
    await frame.locator('#version').filter({hasText:'second'}).waitFor({timeout:20000});
    assert.equal(frame.url(),documentUrl);
    assert.equal(await frame.locator('#counter').innerText(),'1');
    assert.equal(await frame.evaluate(()=>window.reloadSentinel),undefined);
    console.log('PASS: automatic reload, failed/idempotent deployment stability, network recovery, URL and data preservation');
    progress('checking initialized project and configuration watching');
    const projectDir=path.join(directory,'new-project');
    cli(['--profile','starter','init',projectDir]);
    cli(['--project',projectDir,'auth','--webkey-stdin','--allow-local-http'],developer);
    cli(['--project',projectDir,'dev','--once']);
    await frame.locator('h1').filter({hasText:'Hello from your isolate'}).waitFor({timeout:20000});
    const watcher=spawn(path.join(root,'target/debug/spki'),['dev'],{env,cwd:path.join(projectDir,'src'),stdio:['ignore','pipe','pipe']});
    let watchOutput='';
    watcher.stdout.on('data',data=>watchOutput+=data);
    watcher.stderr.on('data',data=>watchOutput+=data);
    async function waitOutput(text) {
      for(let i=0;i<120;i++) {
        if(watchOutput.includes(text)) return;
        if(watcher.exitCode!==null) throw new Error(redact(watchOutput));
        await page.waitForTimeout(100);
      }
      throw new Error('watcher did not report '+text+': '+redact(watchOutput));
    }
    try {
      await waitOutput('Live revision');
      progress('checking live CLI logs through the Sandstorm streaming gateway');
      const workerFile=path.join(projectDir,'src/worker.ts');
      const starter=fs.readFileSync(workerFile,'utf8');
      const logging=starter.replace('async fetch(request: Request) {', `async fetch(request: Request) {
        if(new URL(request.url).pathname === '/log-error') throw new Error('spki-stream-exception');
        console.log('spki-stream-console');`);
      fs.writeFileSync(workerFile,logging);
      await waitOutput('spki-stream-console');
      assert.equal(await frame.evaluate(()=>fetch('/log-error').then(r=>r.status)),500);
      await waitOutput('spki-stream-exception');
      await call('shutdownGrain',grainId);
      await waitOutput('Grain restarted; resuming');
      const beforeLogs=watchOutput.split('spki-stream-console').length;
      await frame.evaluate(()=>fetch('/health'));
      for(let i=0;i<100 && watchOutput.split('spki-stream-console').length===beforeLogs;i++) await page.waitForTimeout(100);
      assert(watchOutput.split('spki-stream-console').length>beforeLogs,'logs did not resume after grain restart');
      console.log('PASS: continuous CLI log stream, caught exception and automatic reconnect after grain restart');
      fs.writeFileSync(path.join(projectDir,'src/greeting.ts'),"export const greeting: string = 'Starter edited';\n");
      await frame.locator('h1').filter({hasText:'Starter edited'}).waitFor({timeout:20000});
      await frame.evaluate(()=>window.configSentinel='preserved');
      const projectFile=path.join(projectDir,'sandstorm-pkgdef.capnp');
      const configuration=fs.readFileSync(projectFile,'utf8');
      fs.writeFileSync(projectFile,configuration.replace('2026-06-10','2000-01-01'));
      await waitOutput('unsupported compatibility date');
      assert.equal(await frame.evaluate(()=>window.configSentinel),'preserved');
      const alternate=fs.readFileSync(path.join(projectDir,'src/worker.ts'),'utf8').replace('<h1>','<h1 data-config="changed">');
      fs.writeFileSync(path.join(projectDir,'src/alternate.ts'),alternate);
      fs.writeFileSync(projectFile,configuration.replace('src/worker.ts','src/alternate.ts'));
      await frame.locator('h1[data-config="changed"]').waitFor({timeout:20000});
      fs.writeFileSync(projectFile,configuration.replace('profile = "starter"','profile = "another"'));
      await waitOutput('preview profile changed');
      for(const secret of secrets) assert(!watchOutput.includes(secret),'watcher leaked a credential');
    } finally {
      watcher.kill('SIGTERM');
      await new Promise(resolve=>{if(watcher.exitCode!==null) resolve();else watcher.once('exit',resolve);});
    }
    console.log('PASS: generated project, profile selection, subdirectory discovery, module/config watching and failed-config preservation');
    if (await page.locator('form.newApiToken').isVisible()) { await page.reload(); frame=await appFrame(); }
    progress('checking browser assets and typed capnp-es RPC');
    fs.cpSync(path.join(root,'preview/fixtures/frontend'), projectDir, {recursive:true});
    const frontendConfig=path.join(projectDir,'sandstorm-pkgdef.capnp');
    let frontendText=fs.readFileSync(frontendConfig,'utf8').replace('profile = "another"','profile = "starter"').replace('src/worker.ts','worker.ts');
    frontendText=frontendText.replace('  compatibilityDate', '  ui = (entrypoints = ["ui/main.ts"]),\n  assets = ["public"],\n  schemas = ["counter.capnp"],\n  compatibilityDate');
    fs.writeFileSync(frontendConfig, frontendText);
    cli(['--project',projectDir,'dev','--once']);
    await frame.goto(new URL('/',frame.url()).href);
    await frame.locator('output').filter({hasText:'0'}).waitFor({timeout:20000});
    await frame.getByRole('button',{name:'Increment',exact:true}).click();
    await frame.locator('output').filter({hasText:'1'}).waitFor();
    assert.equal(await frame.locator('body').evaluate(el=>getComputedStyle(el).color),'rgb(20, 40, 60)');
    const rpcResult=await frame.evaluate(async()=>{
      const results=await Promise.all([counter.increment({amount:2}),counter.increment({amount:3})]);
      return (await counter.read()).value;
    });
    assert.equal(rpcResult,6);
    console.log('PASS: browser TypeScript, CSS, public index, generated schemas and pipelined typed browser RPC');
    progress('revoking deployment webkey through UI');
    await page.getByTitle('Get webkey',{exact:true}).click();
    const row=page.locator('li').filter({hasText:`${label}-developer`}).filter({has:page.getByRole('button',{name:'Revoke',exact:true})}).last();
    await row.getByRole('button',{name:'Revoke',exact:true}).click();
    let denied = false;
    for(let attempt=0;attempt<40;attempt++) {
      try { denied=[401,403,410].includes((await request(developer,'/__spki/v1/info')).status); } catch {}
      if(denied) break;
      await page.waitForTimeout(250);
    }
    assert(denied,'revoked key must return an authorization failure');
    cli(['auth','status'],undefined,false);
    console.log('PASS: real Sandstorm UI webkeys, role restriction, browser/API isolation, CLI deployment, storage/revision restart persistence, revocation');
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
