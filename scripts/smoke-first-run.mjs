import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
const pages = await (await fetch('http://127.0.0.1:9222/json/list')).json();
const page = pages.find(page => page.title === 'Jarvis');
assert(page, 'Start Jarvis without JARVIS_MOCK and with WebView2 debugging on port 9222');
const socket = new WebSocket(page.webSocketDebuggerUrl);
await new Promise((resolve, reject) => { socket.onopen = resolve; socket.onerror = reject; });
let id = 0; const pending = new Map();
socket.onmessage = ({data}) => { const event = JSON.parse(data); if(event.id) { pending.get(event.id)(event); pending.delete(event.id); } };
async function evaluate(expression) {
  const next = ++id;
  const result = await new Promise(resolve => { pending.set(next, resolve); socket.send(JSON.stringify({id: next,method:'Runtime.evaluate',params:{expression,awaitPromise:true,returnByValue:true,userGesture:true}})); });
  assert(!result.error && !result.result.exceptionDetails, JSON.stringify(result));
  return result.result.result.value;
}
const settings = await evaluate(`__TAURI__.core.invoke('bootstrap')`);
assert(!settings.mock, 'This test checks the real first-run path');
assert(!settings.deepgram_saved && !settings.xai_saved, 'Use a fresh profile; this test never removes credentials');
assert.equal(settings.prompt, (await readFile(new URL('../src-tauri/defaults/system_prompt.txt',import.meta.url),'utf8')).trimEnd());
assert(await evaluate(`!document.querySelector('#settings').hidden && document.body.classList.contains('attention')`));
assert(await evaluate(`document.querySelector('#deepgram').value==='' && document.querySelector('#xai').value===''`));
console.log('Real first run: settings auto-opened, exact default prompt, no keys exposed, attention orb.');
if (process.argv.includes('--microphone')) {
  const capture = await evaluate(`(async()=>{
    const stream=await navigator.mediaDevices.getUserMedia({audio:{channelCount:1,echoCancellation:true,noiseSuppression:true,autoGainControl:true},video:false});
    const context=new AudioContext({latencyHint:'interactive'});await context.resume();
    await context.audioWorklet.addModule('./capture-worklet.js');
    const source=context.createMediaStreamSource(stream),node=new AudioWorkletNode(context,'jarvis-capture'),gain=context.createGain();gain.gain.value=0;
    source.connect(node).connect(gain).connect(context.destination);
    const settings=stream.getAudioTracks()[0].getSettings();
    try{return await new Promise((resolve,reject)=>{let frames=0;const timer=setTimeout(()=>reject(Error('No microphone frames')),4000);node.port.onmessage=({data})=>{if(data.byteLength!==640){clearTimeout(timer);reject(Error('Incorrect PCM framing'));}if(++frames>=50){clearTimeout(timer);resolve({frames,inputRate:context.sampleRate,echoCancellation:settings.echoCancellation,noiseSuppression:settings.noiseSuppression,autoGainControl:settings.autoGainControl});}};});}
    finally{stream.getTracks().forEach(t=>t.stop());node.disconnect();await context.close();}
  })()`);
  console.log('Actual microphone -> AudioWorklet, no audio sent to a provider:', capture);
}
socket.close();
