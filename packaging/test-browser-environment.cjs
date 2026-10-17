// Actual installed Edge, synthetic HTTP proxy and local STUN capture; no user profiles or Internet.
const {chromium}=require(process.env.AEC_NODE_MODULES?require('node:path').join(process.env.AEC_NODE_MODULES,'playwright'):'playwright');
const assert=require('node:assert/strict'),fs=require('node:fs'),os=require('node:os'),path=require('node:path');
const http=require('node:http'),dgram=require('node:dgram'),{execFileSync}=require('node:child_process');
const wait=ms=>new Promise(resolve=>setTimeout(resolve,ms));
async function run(){
  const root=fs.mkdtempSync(path.join(os.tmpdir(),'aec-browser-integration-'));
  let context;const seen=[];let packets=0;
  const proxy=http.createServer((req,res)=>{seen.push(req.url);res.writeHead(200,{'Content-Type':'text/html'});res.end('<!doctype html><title>AEC isolated proxy fixture</title><p>fixture</p>')});
  proxy.on('connect',(_req,socket)=>{socket.on('error',()=>{});socket.end('HTTP/1.1 502 Fixture only\r\nConnection: close\r\n\r\n')});
  proxy.on('clientError',(_error,socket)=>socket.destroy());
  let stun=dgram.createSocket('udp4');stun.on('message',()=>packets++);
  await new Promise(resolve=>proxy.listen(0,'127.0.0.1',resolve));await new Promise(resolve=>stun.bind(0,'127.0.0.1',resolve));
  try {
    const fixture=JSON.parse(execFileSync(path.resolve('target/debug/examples/browser_environment_fixture.exe'),[root,String(proxy.address().port)],{encoding:'utf8',windowsHide:true}));
    const flags=fixture.args.filter(flag=>!flag.startsWith('--user-data-dir='));
    const gather=async page=>{
      await page.evaluate(port=>{window.pc=new RTCPeerConnection({iceServers:[{urls:'stun:127.0.0.1:'+port}]});pc.createDataChannel('fixture');return pc.createOffer().then(offer=>pc.setLocalDescription(offer))},stun.address().port);
      await wait(2400);await page.evaluate(()=>pc.close());
    };
    // Positive control proves the capture detects the exact unproxied-UDP failure pattern.
    context=await chromium.launchPersistentContext(path.join(root,'control'),{channel:'msedge',headless:true,args:flags.filter(flag=>!flag.startsWith('--webrtc-ip-handling-policy='))});
    await gather(context.pages()[0]);assert.ok(packets>0,'STUN positive control must observe real UDP before a no-leak assertion is meaningful');
    await context.close();context=null;
    const controlPort=stun.address().port;await new Promise(resolve=>stun.close(resolve));
    stun=dgram.createSocket('udp4');stun.on('message',()=>packets++);await new Promise(resolve=>stun.bind(0,'127.0.0.1',resolve));assert.notEqual(stun.address().port,controlPort);packets=0;
    context=await chromium.launchPersistentContext(fixture.userDataDir,{channel:'msedge',headless:true,args:flags});
    const page=context.pages()[0];await page.goto('http://aec-dns-test.invalid/',{timeout:8000});
    assert.ok(seen.includes('http://aec-dns-test.invalid/'),'unresolvable target must be sent by hostname through the HTTP proxy');
    assert.equal(await page.evaluate(()=>navigator.language),'zh-CN');
    await gather(page);assert.equal(packets,0,'approved AI browser must not send non-proxied STUN UDP');
    await context.close();context=null;
    console.log('Browser environment passed: real Edge remote hostname proxying, Chinese language, positive-control STUN detection and blocked non-proxied STUN; no OS policies or user profiles changed. ECS/global-browser proof remains unknown.');
  } finally {
    if(context)await context.close();await new Promise(resolve=>proxy.close(resolve));stun.close();
    const resolved=path.resolve(root);assert.ok(resolved.startsWith(path.resolve(os.tmpdir())+path.sep)&&path.basename(resolved).startsWith('aec-browser-integration-'));
    fs.rmSync(resolved,{recursive:true,force:true});
  }
}
run().catch(error=>{console.error(error);process.exitCode=1});
