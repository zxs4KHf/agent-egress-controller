// Headless Edge tests against the real presentation server and an isolated fake backend.
const { chromium } = require(process.env.AEC_NODE_MODULES ? require('path').join(process.env.AEC_NODE_MODULES, 'playwright') : 'playwright');
const assert = require('node:assert/strict');
const {spawn} = require('node:child_process');
const path = require('node:path');
const fs = require('node:fs');
const os = require('node:os');
async function run() {
  let child, browser, installation, url = process.argv[2];
  if (!url) {
    installation=fs.mkdtempSync(path.join(os.tmpdir(),'aec-ui-qa-'));
    fs.copyFileSync(path.resolve('target/debug/examples/ui_preview.exe'),path.join(installation,'ui_preview.exe'));
    fs.cpSync(path.resolve('ui'),path.join(installation,'ui'),{recursive:true});
    child=spawn(path.join(installation,'ui_preview.exe'),[],{stdio:['pipe','pipe','inherit'],windowsHide:true});
    url=await new Promise((resolve,reject)=>{let text='';const timer=setTimeout(()=>reject(Error('preview did not start')),15000);child.stdout.on('data',chunk=>{text+=chunk;const match=text.match(/http:\/\/127\.0\.0\.1:\d+\/app\?t=preview&tab=dashboard/);if(match){clearTimeout(timer);resolve(match[0])}});child.on('error',reject);child.on('exit',code=>{if(!text.includes('http://'))reject(Error('preview exited '+code))})});
  }
  try {
    browser=await chromium.launch({channel:'msedge',headless:true});
    const page=await browser.newPage({viewport:{width:1100,height:780}});
    const errors=[];page.on('pageerror',e=>errors.push(e.message));
    await page.goto(url);const frame=id=>page.frameLocator('#'+id);
    page.on('dialog',dialog=>dialog.accept());
    const dashboard=frame('dashboard'),agent=frame('agent'),setup=frame('setup'),help=frame('help');
    await dashboard.locator('#rules tr').first().waitFor();
    assert.equal(await dashboard.locator('#rules tr').count(),12);
    await dashboard.locator('#rules tr').first().locator('input[type=text]').fill('edited.example.test');
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
    console.log('UI passed: filters, pagination/full save, draft preservation, folding/search, two-way Agent toggle, live badge, authenticated navigation, scrollable targets, live resource switch without process restart; screenshots saved.');
  } finally {
    if(browser)await browser.close();
    if(child&&child.exitCode===null){const exited=new Promise(resolve=>child.once('exit',resolve));child.stdin.end('quit\n');await exited;}
    if(installation){const resolved=path.resolve(installation);if(path.dirname(resolved)!==path.resolve(os.tmpdir())||!path.basename(resolved).startsWith('aec-ui-qa-'))throw Error('unsafe fixture cleanup');fs.rmSync(resolved,{recursive:true,force:true});}
  }
}
run().catch(error=>{console.error(error.stack);process.exitCode=1});
