import assert from 'node:assert/strict';
import { mkdir, writeFile } from 'node:fs/promises';
const pages = await (await fetch('http://127.0.0.1:9222/json/list')).json();
const page = pages.find(page => page.title === 'Jarvis');
assert(page, 'Jarvis webview is running');
const socket = new WebSocket(page.webSocketDebuggerUrl);
await new Promise((resolve, reject) => { socket.onopen = resolve; socket.onerror = reject; });
let sequence = 0; const pending = new Map(); const errors = [];
socket.onmessage = ({ data }) => {
  const event = JSON.parse(data);
  if (event.id) { const p = pending.get(event.id); pending.delete(event.id); if (event.error) p.reject(event.error); else p.resolve(event.result); }
  if (event.method === 'Runtime.exceptionThrown') errors.push(event.params.exceptionDetails);
};
function send(method, params = {}) {
  const id = ++sequence;
  return new Promise((resolve, reject) => { pending.set(id, { resolve, reject }); socket.send(JSON.stringify({ id, method, params })); });
}
async function evaluate(expression) {
  const result = await send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true, userGesture: true });
  assert(!result.exceptionDetails, JSON.stringify(result.exceptionDetails)); return result.result.value;
}
async function until(expression, timeout = 15000) {
  return evaluate(`new Promise((resolve,reject)=>{const start=Date.now();const t=setInterval(async()=>{try{const value=await (${expression});if(value){clearInterval(t);resolve(value);}else if(Date.now()-start>${timeout}){clearInterval(t);reject(Error('Timed out waiting for '+${JSON.stringify(expression)}));}}catch(error){clearInterval(t);reject(error)}},25);})`);
}
await send('Runtime.enable');
console.log('Initial UI:', await evaluate(`({state:document.body.className,status:document.querySelector('#status').textContent,secure:isSecureContext})`));
await evaluate(`(async()=>{const s=await __TAURI__.core.invoke('bootstrap');if(!s.mock)throw Error('Smoke test requires JARVIS_MOCK=1');window.__smoke={states:[],chunks:0};for(const state of ['idle','listening','thinking','speaking'])await __TAURI__.event.listen('state://'+state,()=>window.__smoke.states.push(state));return true;})()`);
await evaluate(`__TAURI__.core.invoke('mock_transcript',{text:'Jarvis, hola'})`);
const result = await evaluate(`new Promise((resolve,reject)=>{const start=Date.now();const t=setInterval(()=>{if(window.__smoke.states.includes('speaking')&&document.body.classList.contains('listening')){clearInterval(t);resolve(window.__smoke);}else if(Date.now()-start>12000){clearInterval(t);reject(Error('Audio loop timeout: '+JSON.stringify(window.__smoke)+' '+document.querySelector('#status').textContent));}},50);})`);
assert(result.states.includes('thinking')); assert(result.states.includes('speaking'));
await evaluate(`document.querySelector('#gear').click()`);
await evaluate(`new Promise(r=>setTimeout(r,350))`);
assert(await evaluate(`!document.querySelector('#settings').hidden`));
await mkdir('artifacts', { recursive: true });
const screenshot = await send('Page.captureScreenshot', { format: 'png' });
await writeFile('artifacts/settings.png', Buffer.from(screenshot.data, 'base64'));
await evaluate(`document.dispatchEvent(new KeyboardEvent('keydown',{key:'Escape'}))`);
await evaluate(`new Promise(r=>setTimeout(r,350))`);
const orb = await send('Page.captureScreenshot', { format: 'png' });
await writeFile('artifacts/orb.png', Buffer.from(orb.data, 'base64'));

// Prompt changes cross the real IPC/filesystem path, then restore the exact default.
await evaluate(`document.querySelector('#prompt').value='Prueba de guardado automático.';document.querySelector('#prompt').dispatchEvent(new Event('input'))`);
await until(`__TAURI__.core.invoke('bootstrap').then(s=>s.prompt==='Prueba de guardado automático.')`);
await evaluate(`document.querySelector('#restore').click()`);
await until(`document.querySelector('#prompt').value.startsWith('Eres Jarvis, un asistente de voz personal.')`);
const reports = await evaluate(`__TAURI__.core.invoke('debug_latency')`);
const timing = reports.find(report => report.outcome === 'completed' && 'llm_request_sent' in report.milliseconds_from_speech_end).milliseconds_from_speech_end;
for (const mark of ['speech_end','final_transcript','llm_request_sent','first_token','first_chunk_flushed','tts_request_sent','first_tts_byte','first_audio_played']) assert(mark in timing, mark);
console.log('Offline timing (not provider/hardware latency):', timing);

// Exercise raw binary IPC and the actual sustained-voice gate during playback.
await evaluate(`window.__smoke.stops=[];__TAURI__.event.listen('stop_audio',({payload})=>window.__smoke.stops.push({turn:payload,time:performance.now()}))`);
await evaluate(`__TAURI__.core.invoke('mock_transcript',{text:'Jarvis, prueba de interrupción'})`);
await until(`document.body.classList.contains('speaking')`);
const interruption = await evaluate(`(async()=>{const started=performance.now();const frame=new Int16Array(320);for(let n=0;n<320;n++)frame[n]=Math.sin(n*.1)*4000;for(let n=0;n<12;n++){await __TAURI__.core.invoke('audio_frame',frame.buffer);await new Promise(r=>setTimeout(r,20));}return {started};})()`);
await until(`window.__smoke.stops.length>0`);
const stopped = await evaluate(`window.__smoke.stops.at(-1)`);
assert(stopped.time - interruption.started < 500, 'sustained VAD cancels promptly');
const history = await evaluate(`__TAURI__.core.invoke('debug_history')`);
assert.equal(history.at(-1).role, 'user', 'unfinished sentence is absent from history');
assert(history.at(-1).content.includes('interrupción'));
console.log('Binary IPC -> VAD -> stop_audio:', Math.round(stopped.time - interruption.started), 'ms including the 200 ms voice filter');
await evaluate(`__TAURI__.core.invoke('audio_frame',new ArrayBuffer(640))`);

// New speech while thinking merges into one turn and ignores the cancelled stream.
await evaluate(`(async()=>{await __TAURI__.core.invoke('mock_transcript',{text:'Jarvis, primera parte'});await __TAURI__.core.invoke('mock_transcript',{text:'y segunda parte'});})()`);
await until(`__TAURI__.core.invoke('debug_history').then(h=>h.some(m=>m.role==='user'&&m.content.includes('primera parte')&&m.content.includes('segunda parte')))`);

// Launch a real child process in mock mode, parse its JSONL, and receive a summary.
await evaluate(`__TAURI__.core.invoke('mock_transcript',{text:'Jarvis, crea una app demo'})`);
await until(`__TAURI__.core.invoke('debug_coding_status').then(s=>s.running)`);
await until(`__TAURI__.core.invoke('debug_coding_status').then(s=>!s.running&&s.detail==='Terminado')`);
await until(`__TAURI__.core.invoke('debug_history').then(h=>h.some(m=>m.content?.includes('La prueba del proyecto terminó correctamente')))` , 25000);

// Voice cancellation goes through streaming tool assembly and process-tree kill.
await evaluate(`__TAURI__.core.invoke('mock_transcript',{text:'Jarvis, crea otra app demo'})`);
await until(`__TAURI__.core.invoke('debug_coding_status').then(s=>s.running)`);
await evaluate(`__TAURI__.core.invoke('mock_transcript',{text:'Jarvis, cancela eso'})`);
await until(`__TAURI__.core.invoke('debug_coding_status').then(s=>!s.running&&s.detail==='Cancelado')`);
console.log('Prompt autosave/restore, barge-in history, thinking merge, coding subprocess, spoken summary and voice cancellation passed.');
// Errors must be visible even after the assistant has hidden itself in the tray.
// With background throttling disabled, WebView2 keeps document.hidden false.
// Observe the real native show promise instead of using browser visibility.
await evaluate(`(async()=>{const w=__TAURI__.window.getCurrentWindow();const p=Object.getPrototypeOf(w);window.__smoke.windowPrototype=p;window.__smoke.originalShow=p.show;window.__smoke.shown=false;p.show=async function(){await window.__smoke.originalShow.call(this);window.__smoke.shown=true;};await w.hide();})()`);
await evaluate(`__TAURI__.event.emit('assistant://error','Prueba de recuperación desde la bandeja.')`);
await until(`window.__smoke.shown && !document.querySelector('#settings').hidden && document.querySelector('#status').textContent==='Prueba de recuperación desde la bandeja.'`);
await evaluate(`window.__smoke.windowPrototype.show=window.__smoke.originalShow;document.querySelector('#status').textContent='';document.dispatchEvent(new KeyboardEvent('keydown',{key:'Escape'}))`);
console.log('Runtime errors restore the hidden window and open settings.');
assert.equal(errors.length, 0, JSON.stringify(errors));
console.log('WebView2 offline voice loop and settings passed:', result.states.join(' -> '));
socket.close();
