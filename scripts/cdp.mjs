import assert from 'node:assert/strict';
export async function connect() {
  const pages = await (await fetch('http://127.0.0.1:9222/json/list')).json();
  const page = pages.find(p => p.title === 'Jarvis');
  assert(page, 'Jarvis must run with WebView2 debugging on port 9222');
  const ws = new WebSocket(page.webSocketDebuggerUrl);
  await new Promise((resolve, reject) => { ws.onopen = resolve; ws.onerror = reject; });
  let id = 0; const pending = new Map();
  ws.onmessage = ({ data }) => {
    const m = JSON.parse(data); const p = pending.get(m.id);
    if (p) { pending.delete(m.id); clearTimeout(p.timer); if (m.error || m.result?.exceptionDetails) p.reject(Error(JSON.stringify(m.error || m.result.exceptionDetails))); else p.resolve(m.result); }
  };
  const send = (method, params = {}) => new Promise((resolve, reject) => {
    const next = ++id; const timer = setTimeout(() => { pending.delete(next); reject(Error('CDP evaluation timed out')); }, 30000);
    pending.set(next, { resolve, reject, timer });
    ws.send(JSON.stringify({ id: next, method, params }));
  });
  const evaluate = async (expression, userGesture = false) => (await send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true, userGesture })).result.value;
  const until = expression => evaluate(`new Promise((resolve,reject)=>{const start=Date.now();const timer=setInterval(async()=>{try{const result=await (${expression});if(result){clearInterval(timer);resolve(result);}else if(Date.now()-start>20000){clearInterval(timer);reject(Error('Condition timed out'));}}catch(e){clearInterval(timer);reject(e)}},50)})`);
  return { evaluate, send, until, close: () => ws.close() };
}
