import { readFile } from 'node:fs/promises';
import assert from 'node:assert/strict';
globalThis.AudioWorkletProcessor = class {};
globalThis.registerProcessor = () => {};
const source = await readFile(new URL('../frontend/capture-worklet.js', import.meta.url), 'utf8');
const { PcmResampler } = await import(`data:text/javascript;base64,${Buffer.from(source).toString('base64')}`);
function resample(rate, frequency) {
  const packets = [];
  const r = new PcmResampler(rate, packet => packets.push(new Int16Array(packet)));
  for (let offset = 0; offset < rate; offset += 128) {
    r.push(Float32Array.from({ length: Math.min(128, rate - offset) }, (_, i) => 0.5 * Math.sin(2 * Math.PI * frequency * (offset + i) / rate)));
  }
  assert.equal(packets.length, 50, `one second at ${rate} Hz must yield 50 packets`);
  for (const packet of packets) assert.equal(packet.byteLength, 640);
  return Math.sqrt(packets.slice(1).flatMap(p => Array.from(p)).reduce((sum,x) => sum + (x/32768)**2, 0)/(49*320));
}
for (const rate of [16000, 44100, 48000, 96000]) {
  const speech = resample(rate, 1000);
  assert(speech > 0.3 && speech < 0.4, `speech band preserved at ${rate}`);
  if (rate >= 44100) assert(resample(rate,12000) < 0.025, `anti-alias filter at ${rate}`);
}
console.log('AudioWorklet: 4 sample rates, exact framing, speech passband and alias rejection passed');
