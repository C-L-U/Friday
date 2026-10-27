import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
const source = await readFile(new URL('../frontend/playback.js', import.meta.url), 'utf8');
const { PlaybackScheduler } = await import(`data:text/javascript;base64,${Buffer.from(source).toString('base64')}`);
const nodes = [];
const context = {
  currentTime: 0, outputLatency: 0, destination: {},
  createAnalyser: () => ({ connect() {} }),
  createBuffer: (_, count, rate) => ({ duration: count / rate, getChannelData: () => new Float32Array(count) }),
  createBufferSource() {
    const node = { connect() {}, disconnect() { this.disconnected = true; }, start(time) { this.at = time; }, stop() { this.stopped = true; } };
    nodes.push(node); return node;
  },
};
function packet(turn, sentence, flags, count = 0) {
  const data = new ArrayBuffer(12 + count * 2), view = new DataView(data);
  view.setUint32(0, turn, true); view.setUint32(4, sentence, true); view.setUint32(8, flags, true);
  return data;
}
const reports = [];
const p = new PlaybackScheduler(context, event => reports.push(event));
p.accept(packet(1, 1, 0, 2400));
p.accept(packet(1, 1, 0, 2400));
assert.equal(nodes[0].at, 0.018);
assert(Math.abs(nodes[1].at - 0.118) < 1e-12, 'contiguous PCM has no scheduling gap');
p.accept(packet(1, 1, 1));
p.accept(packet(1, 0, 2));
p.stop(1);
assert(nodes.every(node => node.stopped && node.disconnected));
assert.equal(p.timers.size, 0, 'barge-in removes pending playback acknowledgements');
p.accept(packet(1, 2, 0, 2400));
assert.equal(nodes.length, 2, 'late cancelled packets cannot restart audio');
p.accept(packet(2, 1, 0, 2400));
p.stop(1);
assert.equal(nodes[2].stopped, undefined, 'old cancellation cannot stop a newer turn');
context.currentTime = 1;
await new Promise(resolve => setTimeout(resolve, 30));
p.accept(packet(2, 1, 1)); p.accept(packet(2, 0, 2));
assert.deepEqual(reports.map(event => event.kind), ['started', 'sentence', 'ended']);
p.stop(2);
assert.throws(() => p.accept(new ArrayBuffer(13)), /inválido/);
console.log('Playback: contiguous scheduling, ordered acknowledgements, barge-in and stale packets passed.');
