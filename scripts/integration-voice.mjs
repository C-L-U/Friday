// Run against an installed app launched with JARVIS_MOCK=1 and CDP port 9222.
// No mock_transcript: actual PCM IPC -> VAD -> MockStt -> turn -> MockLlm ->
// ordered MockTts -> real binary Channel -> production PlaybackScheduler.
import assert from 'node:assert/strict';
import { connect } from './cdp.mjs';
const c = await connect();
try {
  const result = await c.evaluate(`(async () => {
    const settings = await __TAURI__.core.invoke('bootstrap');
    if (!settings.mock) throw Error('Integration test requires JARVIS_MOCK=1');
    const { PlaybackScheduler } = await import('./playback.js');
    const context = new AudioContext(); await context.resume();
    const states = [], unlisten = [];
    for (const state of ['listening', 'thinking', 'speaking', 'idle']) unlisten.push(await __TAURI__.event.listen('state://' + state, () => states.push(state)));
    let packets = 0, samples = 0, nonzero = 0, reports = Promise.resolve();
    const player = new PlaybackScheduler(context, event => { reports = reports.then(() => __TAURI__.core.invoke('playback_event', event)); });
    const channel = new __TAURI__.core.Channel();
    let packetError;
    channel.onmessage = data => {
      if (!(data instanceof ArrayBuffer) || data.byteLength < 12 || data.byteLength % 2) { packetError = 'misaligned or nonbinary PCM'; return; }
      const view = new DataView(data); packets++;
      for (let i = 12; i < data.byteLength; i += 2) { samples++; if (view.getInt16(i, true)) nonzero++; }
      player.accept(data);
    };
    await __TAURI__.core.invoke('connect_audio', { onAudio: channel });
    // Return to idle first so listening is observed from real VAD.
    await new Promise(resolve => setTimeout(resolve, 11000)); states.length = 0;
    for (let frame = 0; frame < 45; frame++) {
      const data = new ArrayBuffer(640), view = new DataView(data);
      for (let i = 0; i < 320; i++) view.setInt16(i * 2, frame < 20 ? Math.round(Math.sin((frame * 320 + i) * Math.PI * 2 * 220 / 16000) * 5000) : 0, true);
      await __TAURI__.core.invoke('audio_frame', data);
      await new Promise(resolve => setTimeout(resolve, 20));
    }
    const deadline = performance.now() + 14000;
    while (!(states.includes('speaking') && states.at(-1) === 'idle')) {
      if (performance.now() > deadline) throw Error('Pipeline timeout ' + JSON.stringify(states));
      await new Promise(resolve => setTimeout(resolve, 50));
    }
    await reports;
    const diagnostics = await __TAURI__.core.invoke('diagnostics_snapshot');
    for (const off of unlisten) off(); player.stop(); await context.close();
    return { states, packets, samples, nonzero, packetError, transcript: diagnostics.transcript };
  })()`);
  assert(!result.packetError, result.packetError);
  assert(result.packets > 2 && result.samples > 0 && result.nonzero > 0);
  const sequence = ['listening', 'thinking', 'speaking', 'idle'];
  let index = 0;
  for (const state of result.states) if (state === sequence[index]) index++;
  assert.equal(index, sequence.length, JSON.stringify(result.states));
  assert.match(result.transcript, /hola/i);
  console.log('Installed mock PCM integration passed:', result);
} finally { await c.send('Page.reload'); c.close(); }
