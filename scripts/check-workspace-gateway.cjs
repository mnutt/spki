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
  const env = {...process.env,XDG_CONFIG_HOME:path.join(directory,'config')};
  const browser = await chromium.launch({headless:true,executablePath:process.env.CHROMIUM_PATH || '/usr/bin/chromium',args:['--host-resolver-rules=MAP *.local.sandstorm.io 127.0.0.1, MAP local.sandstorm.io 127.0.0.1','--no-proxy-server']});
  const context = await browser.newContext();
  // These checks navigate explicitly; automatic reload can destroy evaluate() mid-assertion.
  await context.route(url=>url.pathname==='/__spki/v1/reload.js', route=>route.fulfill({status:200,contentType:'application/javascript',body:''}));
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
    async function appFrame(shell=page) {
      await shell.waitForFunction(()=>[...document.querySelectorAll('iframe')].some(f=>f.src.includes('.local.sandstorm.io')));
      for(let n=0;n<120;n++) {
        const frame=shell.frames().find(f=>f.url().includes('.local.sandstorm.io') && f.url()!==shell.url());
        if(frame && await frame.locator('body').count()) return frame;
        await shell.waitForTimeout(250);
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
    progress('creating two projects with one saved credential');
    const projects=[];
    for(const name of ['alpha','beta']) {
      const dir=path.join(directory,name);
      cli(['init',dir]);
      const config=fs.readFileSync(path.join(dir,'sandstorm-pkgdef.capnp'),'utf8');
      const id=config.match(/project = "([^"]+)"/)[1];
      const source=fs.readFileSync(path.join(root,'preview/fixtures/sdk-counter.js'),'utf8')
        .replace('const api = sandstorm(request, env);',`console.log('${name} request'); const api = sandstorm(request, env);`)
        .replace('Response.json({count})',`Response.json({count,name:'${name}',generation:1})`);
      fs.writeFileSync(path.join(dir,'src/worker.ts'),source);
      cli(['--project',dir,'dev','--once']);
      projects.push({name,dir,id});
    }
    const [alpha,beta]=projects;
    const listed=await request(developer,'/__spki/v1/projects');
    assert.deepEqual(JSON.parse(listed.body).sort(),projects.map(p=>p.id).sort());
    assert.equal((await request(viewer,'/__spki/v1/projects')).status,403);
    assert.equal(await frame.evaluate(()=>fetch('/__spki/v1/projects/unauthorized/create',{method:'POST',body:'{}'}).then(r=>r.status)),403);
    assert.notEqual((await request(viewer,`/__spki/v1/projects/${alpha.id}/__spki/v1/info`)).status,200);
    await frame.goto(new URL('/__spki/workspace',frame.url()).href);
    assert(await frame.getByRole('link',{name:alpha.id,exact:true}).isVisible());
    const open=(frame,project)=>require('./workspace-frame.cjs')(frame,project.id);
    const json=(frame,route)=>frame.evaluate(route=>fetch(route).then(r=>r.json()),route);
    frame=await open(frame,alpha);
    assert.deepEqual(await json(frame,'/increment'),{count:1,name:'alpha',generation:1});
    progress('checking persistent toolbar, message relay and deep links');
    let workspace=frame.parentFrame();
    assert.equal(await workspace.locator('#project').inputValue(),alpha.id);
    await workspace.evaluate(()=>window.toolbarMarker='preserved');
    const appLoaded=page.waitForEvent('framenavigated',{predicate:f=>f===frame});
    await workspace.getByRole('button',{name:'Reload',exact:true}).click();
    await appLoaded;
    assert.equal(await workspace.evaluate(()=>window.toolbarMarker),'preserved');
    const title=await frame.evaluate(()=>new Promise((resolve,reject)=>{
      const timer=setTimeout(()=>reject(new Error('getGrainTitle relay timed out')),5000);
      const receive=event=>{
        if(event.source===parent && event.data?.rpcId==='workspace-test'){
          clearTimeout(timer);removeEventListener('message',receive);resolve(event.data.grainTitle);
        }
      };
      addEventListener('message',receive);
      parent.postMessage({getGrainTitle:{},rpcId:'workspace-test'},location.origin);
    }));
    assert.equal(typeof title,'string');
    await frame.evaluate(()=>parent.postMessage({setPath:'/route?hello=world#fragment'},location.origin));
    const encodedPath=Buffer.from('/route?hello=world#fragment').toString('base64url');
    await workspace.waitForFunction(encoded=>new URL(location.href).searchParams.get('path')===encoded,encodedPath);
    await page.waitForURL(url=>url.href.includes('path='));
    await page.reload();
    workspace=await appFrame();
    await workspace.locator('#project-preview').waitFor();
    frame=await (await workspace.locator('#project-preview').elementHandle()).contentFrame();
    await frame.waitForURL(url=>url.href!=='about:blank');
    assert.equal(new URL(frame.url()).pathname,'/route', 'restored iframe URL: '+frame.url()+'; workspace: '+workspace.url());
    assert.equal(new URL(frame.url()).hash,'#fragment',JSON.stringify({frame:frame.url(),workspace:workspace.url(),src:await workspace.locator('#project-preview').getAttribute('data-initial-path'),page:page.url()}));
    assert.deepEqual(await json(frame,'/'),{count:1,name:'alpha',generation:1});
    await frame.evaluate(()=>parent.postMessage({setPath:'//example.com'},location.origin));
    await page.waitForTimeout(100);
    assert.equal(new URL(workspace.url()).searchParams.get('path'),encodedPath);

    const other=await context.newPage();
    await other.goto(origin+'/grain/'+grainId);
    let otherFrame=await appFrame(other);
    otherFrame=await open(otherFrame,beta);
    assert.deepEqual(await json(otherFrame,'/'),{count:0,name:'beta',generation:1});
    assert.deepEqual(await json(otherFrame,'/increment'),{count:1,name:'beta',generation:1});
    assert.deepEqual(await json(frame,'/increment'),{count:2,name:'alpha',generation:1});
    // Switch through the actual selector, not just the open-project endpoint.
    workspace=frame.parentFrame();
    await workspace.locator('#project').selectOption(beta.id);
    await workspace.waitForURL(url=>url.pathname.endsWith('/'+beta.id));
    await workspace.locator('#project-preview').waitFor();
    frame=await (await workspace.locator('#project-preview').elementHandle()).contentFrame();
    await frame.waitForURL(url=>url.href!=='about:blank');
    assert.deepEqual(await json(frame,'/'),{count:1,name:'beta',generation:1});
    frame=await open(frame,alpha);

    progress('checking independent replacement and log subscriptions');
    let alphaLogs='',betaLogs='';
    const followers=projects.map((p,i)=>{
      const child=spawn(path.join(root,'target/debug/spki'),['--project',p.dir,'dev'],{env,stdio:['ignore','pipe','pipe']});
      child.stdout.on('data',d=>i===0?alphaLogs+=d:betaLogs+=d);
      child.stderr.on('data',d=>i===0?alphaLogs+=d:betaLogs+=d);
      return child;
    });
    try {
      for(let i=0;i<150 && (!alphaLogs.includes('Live revision') || !betaLogs.includes('Live revision'));i++)await page.waitForTimeout(100);
      assert(alphaLogs.includes('Live revision') && betaLogs.includes('Live revision'),redact(alphaLogs+betaLogs));
      await json(frame,'/');await json(otherFrame,'/');
      for(let i=0;i<50 && (!alphaLogs.includes('alpha request') || !betaLogs.includes('beta request'));i++)await page.waitForTimeout(100);
      assert(alphaLogs.includes('alpha request') && !alphaLogs.includes('beta request'),redact(alphaLogs));
      assert(betaLogs.includes('beta request') && !betaLogs.includes('alpha request'),redact(betaLogs));
      const worker=path.join(alpha.dir,'src/worker.ts');
      fs.writeFileSync(worker,fs.readFileSync(worker,'utf8').replace('generation:1','generation:2'));
      for(let i=0;i<100;i++){if((await json(frame,'/')).generation===2)break;await page.waitForTimeout(100);}
      assert.deepEqual(await json(frame,'/'),{count:2,name:'alpha',generation:2});
      assert.deepEqual(await json(otherFrame,'/'),{count:1,name:'beta',generation:1});
    } finally {
      for(const child of followers)child.kill('SIGINT');
      await Promise.all(followers.map(child=>new Promise(resolve=>child.exitCode!==null?resolve():child.once('exit',resolve))));
    }
    progress('checking restart persistence and lazy project restoration');
    await call('shutdownGrain',grainId);
    for(let i=0;i<50;i++){try{if((await request(developer,'/__spki/v1/info')).status===200)break;}catch{}await page.waitForTimeout(200);}
    await page.reload();frame=await appFrame();
    frame=await open(frame,alpha);
    assert.deepEqual(await json(frame,'/'),{count:2,name:'alpha',generation:2});
    await other.reload();otherFrame=await appFrame(other);
    otherFrame=await open(otherFrame,beta);
    assert.deepEqual(await json(otherFrame,'/'),{count:1,name:'beta',generation:1});
    frame=await open(frame,{id:''});
    assert.equal((await json(frame,'/')).count,1);
    console.log('PASS: one auth, separate project code/storage/logs/sessions, replacement, restart, permissions and legacy data');
  } finally {
    try { if(grainId) { console.log('cleaning up test grain'); await call('moveGrainsToTrash',[grainId]); } }
    finally { await browser.close(); fs.rmSync(directory,{recursive:true,force:true}); }
  }
})().catch(error=>{console.error(`FAIL (${stage}): ${redact(String(error))}`);process.exit(1)});
