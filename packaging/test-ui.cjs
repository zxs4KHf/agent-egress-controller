// Headless Edge tests against the real presentation server and an isolated fake backend.
const { chromium } = require(process.env.AEC_NODE_MODULES ? require('path').join(process.env.AEC_NODE_MODULES, 'playwright') : 'playwright');
const assert = require('node:assert/strict');
const {spawn} = require('node:child_process');
const path = require('node:path');
const fs = require('node:fs');
const os = require('node:os');
async function run() {
  let child, browser, page, installation, url = process.argv[2];
  const transportFailures=[];
  if (!url) {
    installation=fs.mkdtempSync(path.join(os.tmpdir(),'aec-ui-qa-'));
    fs.copyFileSync(path.resolve('target/debug/examples/ui_preview.exe'),path.join(installation,'ui_preview.exe'));
    fs.cpSync(path.resolve('ui'),path.join(installation,'ui'),{recursive:true});
    child=spawn(path.join(installation,'ui_preview.exe'),[],{stdio:['pipe','pipe','inherit'],windowsHide:true});
    url=await new Promise((resolve,reject)=>{let text='';const timer=setTimeout(()=>reject(Error('preview did not start')),15000);child.stdout.on('data',chunk=>{text+=chunk;const match=text.match(/http:\/\/127\.0\.0\.1:\d+\/app\?t=preview&tab=dashboard/);if(match){clearTimeout(timer);resolve(match[0])}});child.on('error',reject);child.on('exit',code=>{if(!text.includes('http://'))reject(Error('preview exited '+code))})});
  }
  try {
    browser=await chromium.launch({channel:'msedge',headless:true});
    page=await browser.newPage({viewport:{width:1100,height:780}});
    // Diagnostics are confined to the synthetic installation; never capture user pages/tokens.
    if(installation){
      page.on('requestfailed',r=>transportFailures.push({path:new URL(r.url()).pathname,error:r.failure()?.errorText}));
      page.on('response',r=>{if(r.status()>=400)transportFailures.push({path:new URL(r.url()).pathname,status:r.status()})});
    }
    const errors=[];page.on('pageerror',e=>errors.push(e.message));
    await page.goto(url);const frame=id=>page.frameLocator('#'+id);
    let approveDialog=true;page.on('dialog',dialog=>approveDialog?dialog.accept():dialog.dismiss());
    const dashboard=frame('dashboard'),agent=frame('agent'),setup=frame('setup'),help=frame('help');
    await dashboard.locator('#rules tr').first().waitFor();
    assert.equal(await dashboard.locator('#environmentScope').count(),1,'browser settings must expose AI-only and global scopes');
    await dashboard.locator('#environmentPanel > summary').click();
    await dashboard.locator('#environmentScope').selectOption('global');
    await dashboard.locator('#environmentLanguage').selectOption('zh-CN');
    await dashboard.locator('#environmentPreview').click();
    await dashboard.locator('#environmentPlan').filter({visible:true}).waitFor();
    assert.match(await dashboard.locator('#environmentPlan').innerText(),/Chrome/,'fingerprint Chrome exclusion must appear before confirmation');
    assert.match(await dashboard.locator('#environmentPlan').innerText(),/QUIC \/ HTTP\/3/,'preview must explain changes in user-facing language');
    assert.match(await dashboard.locator('#environmentPlan').innerText(),/未配置/,'absent original policy must not be shown as an unexplained null');
    if(installation){fs.mkdirSync(path.resolve('.scratch/ui-qa'),{recursive:true});await dashboard.locator('#environmentPanel').screenshot({path:path.resolve('.scratch/ui-qa/browser-environment.png')})}
    assert.equal(await dashboard.locator('#environmentApply').isDisabled(),false);
    await dashboard.locator('#environmentLanguage').selectOption('en-US');
    assert.equal(await dashboard.locator('#environmentApply').isDisabled(),true,'editing options invalidates the confirmed plan');
    await dashboard.locator('#environmentPreview').click();
    await dashboard.locator('#environmentApply').filter({visible:true}).waitFor();
    approveDialog=false;await dashboard.locator('#environmentApply').click();approveDialog=true;
    assert.equal(await dashboard.locator('#environmentRestore').isDisabled(),true,'cancel must not change browser settings');
    await dashboard.locator('#environmentApply').click();
    await dashboard.locator('#environmentNotice').filter({hasText:'已保存'}).waitFor();
    assert.match(await dashboard.locator('#environmentNotice').innerText(),/未验证/);
    await dashboard.locator('#environmentRestore').click();
    await dashboard.locator('#environmentNotice').filter({hasText:'已恢复'}).waitFor();
    await dashboard.locator('#environmentScope').selectOption('ai_only');
    await dashboard.locator('#environmentPreview').click();
    assert.match(await dashboard.locator('#environmentPlan').innerText(),/仅 AEC/);
    await dashboard.locator('#environmentPanel > summary').click();
    assert.equal(await dashboard.locator('#exportCooperation').count(),1,'selected-site cooperation must have a working explicit preview/export entry');
    await dashboard.locator('#exportCooperation').evaluate(el=>el.closest('details').open=true);
    await dashboard.locator('#exportCooperation').click();
    await dashboard.locator('#cooperationCode textarea').filter({visible:true}).waitFor();
    assert.match(await dashboard.locator('#cooperationScript').inputValue(),/AEC-MANAGED-FIXED/);
    assert.match(await dashboard.locator('#cooperationNotice').innerText(),/未修改或重载/);
    const coop=await page.request.get(new URL('/cooperation?t=preview',url).href);
    const coopView=await coop.json();assert.equal(coopView.runtimeVerified,false);assert.equal(coopView.persistentInstalled,null);assert.equal(coopView.installedByAec,false);
    assert.ok([403,404].includes((await page.request.get(new URL('/cooperation?t=wrong',url).href)).status()),'unauthenticated export must not expose script data');
    await dashboard.locator('#cooperationPreview').click();
    await dashboard.locator('#cooperationPlan').filter({visible:true}).waitFor();
    assert.match(await dashboard.locator('#cooperationPlan').innerText(),/不自动激活/);
    approveDialog=false;await dashboard.locator('#cooperationInstall').click();approveDialog=true;
    assert.equal(await dashboard.locator('#cooperationRestore').isDisabled(),true,'cancelled installation must not acquire ownership');
    await dashboard.locator('#cooperationInstall').click();
    await dashboard.locator('#cooperationNotice').filter({hasText:'等待你在 Clash'}).waitFor();
    assert.match(await dashboard.locator('#cooperationBadge').innerText(),/未验证/);
    await dashboard.locator('#cooperationRestore').click();
    await dashboard.locator('#cooperationNotice').filter({hasText:'已移除'}).waitFor();
    await dashboard.locator('#cooperationPreview').click();
    await dashboard.locator('#cooperationPlan').filter({visible:true}).waitFor();
    assert.equal(await dashboard.locator('#rules tr').count(),12);
    await dashboard.locator('#rules tr').first().locator('input[type=text]').fill('edited.example.test');
    assert.equal(await dashboard.locator('#cooperationInstall').isDisabled(),true,'editing a rule must invalidate a pending cooperation installation');
    assert.equal(await dashboard.locator('#copyCooperation').isDisabled(),true,'editing the protected scope must invalidate a previously exported script');
    assert.equal(await dashboard.getByRole('button',{name:'Preview GPT → DIRECT（非固定出口）',exact:true}).isDisabled(),true,'unsaved edits must immediately disable quick switching');
    await page.waitForTimeout(5200);
    assert.equal(await dashboard.locator('#rules tr').first().locator('input[type=text]').inputValue(),'edited.example.test','polling must not overwrite a draft');
    await dashboard.locator('#targetFilter').selectOption('direct');
    assert.equal(await dashboard.locator('#rules tr').count(),12);
    assert.match(await dashboard.locator('#pageInfo').innerText(),/25 条匹配/);
    assert.equal(await dashboard.locator('#rules tr').first().locator('select').nth(1).inputValue(),'DIRECT');
    await dashboard.locator('#nextPage').click();
    assert.match(await dashboard.locator('#pageInfo').innerText(),/第 2 \/ 3 页/);
    await dashboard.locator('#nextPage').click();assert.equal(await dashboard.locator('#rules tr').count(),1);
    const saved=page.waitForRequest(r=>r.method()==='POST'&&new URL(r.url()).pathname==='/rules');
    const savedResponse=page.waitForResponse(r=>r.request().method()==='POST'&&new URL(r.url()).pathname==='/rules');
    await dashboard.locator('#save').click();
    const body=(await saved).postDataJSON();assert.equal(body.rules.length,76,'save must include hidden pages');assert.equal(body.rules[0].value,'edited.example.test');
    const result=await (await savedResponse).json();assert.equal(result.ok,true,JSON.stringify(result));
    await dashboard.locator('#draftStatus').filter({hasText:'站点与规则已保存'}).waitFor();
    await dashboard.getByRole('button',{name:'Preview GPT → DIRECT（非固定出口）',exact:true}).click();
    await dashboard.locator('#notice').filter({hasText:'操作已完成'}).waitFor();
    assert.equal(await dashboard.getByRole('button',{name:'Preview GPT → DIRECT（非固定出口）',exact:true}).getAttribute('aria-pressed'),'true');
    await dashboard.getByRole('button',{name:'Preview GPT → relay（固定 ISP）',exact:true}).click();
    await dashboard.locator('#notice').filter({hasText:'操作已完成'}).waitFor();
    assert.ok(await dashboard.locator('.site-item').filter({hasText:'chatgpt.com'}).count(),'fixed switch restores site protection');
    const protectedBefore=await dashboard.locator('.site-item .domain').allTextContents();
    await dashboard.locator('#upstreamsPanel > summary').click();
    await dashboard.locator('#newUpstream').click();
    await dashboard.locator('#upstreamName').fill('Clash');
    await dashboard.locator('#upstreamPort').fill('7890');
    const upstreamResponse=page.waitForResponse(r=>r.request().method()==='POST'&&new URL(r.url()).pathname==='/upstream');
    await dashboard.locator('#saveUpstream').click();
    assert.equal((await (await upstreamResponse).json()).ok,true);
    await dashboard.locator('[data-upstream="Clash"]').waitFor();
    assert.match(await dashboard.locator('#upstreamSummary').innerText(),/xxyun/,'adding must preserve XxYun selection');
    await dashboard.locator('[data-upstream="Clash"]').getByRole('button',{name:'修改',exact:true}).click();
    await dashboard.locator('#upstreamPort').fill('7891');await dashboard.locator('#saveUpstream').click();
    await dashboard.locator('[data-upstream="Clash"]').filter({hasText:'7891'}).waitFor();
    await dashboard.locator('[data-upstream="Clash"]').getByRole('button',{name:'检测',exact:true}).click();
    await dashboard.locator('#upstreamEvidence').filter({hasText:'PREVIEW_NO_NETWORK'}).waitFor();
    assert.match(await dashboard.locator('#upstreamSummary').innerText(),/xxyun/,'probe must not select');
    await dashboard.locator('[data-upstream="Clash"]').getByRole('button',{name:'使用',exact:true}).click();
    await dashboard.locator('#upstreamSummary').filter({hasText:'Clash'}).waitFor();
    assert.deepEqual(await dashboard.locator('.site-item .domain').allTextContents(),protectedBefore,'upstream switch never changes protected sites');
    await dashboard.locator('[data-upstream="xxyun"]').getByRole('button',{name:'使用',exact:true}).click();
    await dashboard.locator('#upstreamSummary').filter({hasText:'xxyun'}).waitFor();
    await dashboard.locator('[data-upstream="Clash"]').getByRole('button',{name:'删除',exact:true}).click();
    await dashboard.locator('[data-upstream="Clash"]').waitFor({state:'detached'});
    await dashboard.locator('#rulesPanel summary').click();assert.equal(await dashboard.locator('#rulesPanel').getAttribute('open'),null);
    await dashboard.locator('#rulesPanel summary').click();
    await dashboard.locator('#sitesPanel summary').first().click();assert.equal(await dashboard.locator('#sitesPanel').getAttribute('open'),null);
    await dashboard.locator('#sitesPanel summary').first().click();
    const originalSites=await dashboard.locator('.site-item').count();
    await dashboard.locator('#site').fill('sample.example.com');await dashboard.locator('#addSite').click();
    assert.equal(await dashboard.locator('.site-item').count(),originalSites+1);
    await dashboard.locator('#siteSearch').fill('sample.example.com');assert.equal(await dashboard.locator('.site-item').count(),1);
    await page.locator('[data-tab=agent]').click();
    await agent.locator('#toggle').click();await agent.locator('#status').filter({hasText:'已开启'}).waitFor();
    assert.match(await page.locator('#agentBadge').innerText(),/已开启/);
    await agent.locator('#toggle').click();await agent.locator('#status').filter({hasText:'已关闭'}).waitFor();
    assert.match(await page.locator('#agentBadge').innerText(),/已关闭/);
    await agent.locator('[data-nav=dashboard]').click();await page.locator('#dashboard.active').waitFor();
    await dashboard.locator('[data-nav=setup]').click();await page.locator('#setup.active').waitFor();
    assert.equal(await setup.locator('#targets').evaluate(el=>el.closest('details').open),false);
    assert.ok(await setup.locator('#targets').evaluate(el=>parseFloat(getComputedStyle(el).maxHeight)<=160));
    await page.locator('[data-tab=help]').click();await help.locator('[data-nav=setup]').last().click();
    await page.locator('#setup.active').waitFor();
    await page.evaluate(()=>window.postMessage({type:'aec:navigate',tab:'help'},location.origin));
    assert.match(await page.locator('#setup').getAttribute('class'),/active/,'messages from the shell must not impersonate an iframe');
    await setup.locator('[data-nav=agent]').click();await page.locator('#agent.active').waitFor();
    if(installation){
      const uiRoot=path.join(installation,'ui');const manifest=JSON.parse(fs.readFileSync(path.join(uiRoot,'active.json'),'utf8'));
      const next=path.join(uiRoot,'revisions','qa-next');fs.cpSync(path.join(uiRoot,'revisions',manifest.revision),next,{recursive:true});
      const helpPath=path.join(next,'help.html');fs.writeFileSync(helpPath,fs.readFileSync(helpPath,'utf8').replace('</main>','<p id="hotUpdated">界面热更新验收</p></main>'));
      const pointer=path.join(uiRoot,'next.json');fs.writeFileSync(pointer,JSON.stringify({schemaVersion:2,revision:'qa-next'}));fs.renameSync(pointer,path.join(uiRoot,'active.json'));
      const helpUrl=new URL('/proxy-help?t=preview',url).href;const response=await page.request.get(helpUrl);const updatedHtml=await response.text();assert.ok(updatedHtml.includes('界面热更新验收'),'live server must read the new presentation revision');assert.ok(updatedHtml.includes('/ui/qa-next/theme.css'),'new HTML must pin its own resource revision');
      assert.ok((await page.request.get(new URL('/ui/'+manifest.revision+'/theme.css?t=preview',url).href)).ok(),'old page assets remain available after pointer switch');
      assert.equal(child.exitCode,null,'presentation change must not restart the running server');
      await page.locator('[data-tab=dashboard]').click();await dashboard.locator('#updatesPanel > summary').click();
      await dashboard.locator('#updatesPanel .compact-details > summary').click();
      assert.match(await dashboard.locator('#updatesPanel .compact-details').innerText(),/aec_update_status/);
      assert.match(await dashboard.locator('#updatesPanel .compact-details').innerText(),/update-status/);
      const updateResponse=page.waitForResponse(r=>r.request().method()==='POST'&&new URL(r.url()).pathname==='/update');await dashboard.locator('#updateUi').click();assert.equal((await (await updateResponse).json()).ok,true);
      await dashboard.locator('#updatesPanel > summary').click();
    }
    await page.locator('[data-tab=dashboard]').click();await dashboard.locator('#siteSearch').fill('');await dashboard.locator('#targetFilter').selectOption('all');
    const screenshotDir=process.env.AEC_UI_SCREENSHOT_DIR||'.scratch/ui-qa';fs.mkdirSync(screenshotDir,{recursive:true});
    await dashboard.locator('main').evaluate(el=>el.scrollIntoView({block:'start'}));
    await page.screenshot({path:path.join(screenshotDir,'dashboard.png')});
    await page.locator('[data-tab=agent]').click();await page.screenshot({path:path.join(screenshotDir,'agent.png')});
    await page.setViewportSize({width:600,height:780});await page.screenshot({path:path.join(screenshotDir,'compact.png')});
    assert.deepEqual(errors,[],'page JavaScript errors');
    console.log('UI passed: selected-site export, two browser scopes/preview/invalidated plan/cancel/confirm/restore, filters, pagination/full save, draft preservation, folding/search, two-way Agent toggle, upstream CRUD, authenticated navigation and live UI hot update; screenshots saved.');
  } catch(error) {
    if(installation&&page){
      const dir=process.env.AEC_UI_SCREENSHOT_DIR||'.scratch/ui-qa';fs.mkdirSync(dir,{recursive:true});
      const diagnostics={transportFailures:transportFailures.slice(-30),frames:[]};
      for(const f of page.frames()){
        try{diagnostics.frames.push({path:new URL(f.url()).pathname,body:(await f.locator('body').innerText({timeout:1500})).slice(0,3000)})}
        catch{diagnostics.frames.push({unreadable:true})}
      }
      fs.writeFileSync(path.join(dir,'failure.json'),JSON.stringify(diagnostics,null,2));
      await page.screenshot({path:path.join(dir,'failure.png'),timeout:5000}).catch(()=>{});
      console.error('Isolated UI failure evidence:',JSON.stringify(diagnostics));
    }
    throw error;
  } finally {
    if(browser)await browser.close();
    if(child&&child.exitCode===null){const exited=new Promise(resolve=>child.once('exit',resolve));child.stdin.end('quit\n');await exited;}
    if(installation){const resolved=path.resolve(installation);if(path.dirname(resolved)!==path.resolve(os.tmpdir())||!path.basename(resolved).startsWith('aec-ui-qa-'))throw Error('unsafe fixture cleanup');fs.rmSync(resolved,{recursive:true,force:true});}
  }
}
run().catch(error=>{console.error(error.stack);process.exitCode=1});
