import assert from 'node:assert/strict';
import { connect } from './cdp.mjs';
const c = await connect();
try {
  const state = await c.evaluate(`__TAURI__.core.invoke('bootstrap').then(s=>({mock:s.mock,wake:s.config.activation.require_wake_word,deepgram:s.deepgram_saved,xai:s.xai_saved}))`);
  console.log('Presence and activation:', state);
  assert(state.mock, 'Use isolated mock mode for reproduction');
  await c.evaluate(`__TAURI__.core.invoke('mock_transcript',{text:'hola'})`);
  await c.until(`__TAURI__.core.invoke('diagnostics_snapshot').then(d=>d.wake?.startsWith('rejected'))`);
  console.log('Wake decision:', await c.evaluate(`__TAURI__.core.invoke('diagnostics_snapshot').then(d=>({wake:d.wake,audio_context:d.audio_context,phase:d.phase}))`));
  await c.evaluate(`document.querySelector('#deepgram').value='diagnostic-synthetic-deepgram';document.querySelector('#xai').value='diagnostic-synthetic-xai';document.querySelector('#form').requestSubmit()`);
  await c.until(`!document.querySelector('#save').disabled`);
  console.log('After successful save:', await c.evaluate(`({empty:document.querySelector('#deepgram').value==='',placeholder:document.querySelector('#deepgram').placeholder,status:document.querySelector('#status').textContent,validated:document.querySelector('#dg-dot').classList.contains('good')})`));
  await c.evaluate(`__TAURI__.core.invoke('mock_transcript',{text:'Jarvis, hola'})`);
  await c.until(`__TAURI__.core.invoke('debug_latency').then(r=>r.some(x=>x.milliseconds_from_speech_end.first_audio_played!==undefined))`);
  console.log('Playback probe:', await c.evaluate(`__TAURI__.core.invoke('diagnostics_snapshot').then(d=>({playback:d.playback,pcm_sent:d.pcm_sent,audio_context:d.audio_context,logs:d.logs}))`));
} finally { c.close(); }
