import { connect } from './cdp.mjs';
const c = await connect();
try {
  const status = await c.evaluate(`__TAURI__.core.invoke('bootstrap').then(s=>({deepgram:s.deepgram_saved,xai:s.xai_saved,model:s.config.llm.model,tts:s.config.tts.model,wake:s.config.activation.require_wake_word}))`);
  console.log('Stored credential presence (no values):', status);
  console.log('Runtime diagnostics:', await c.evaluate(`__TAURI__.core.invoke('diagnostics_snapshot')`));
  console.log('Device support:', await c.evaluate(`({sink:typeof AudioContext.prototype.setSinkId, speech:typeof speechSynthesis, secure:isSecureContext})`));
  if (status.deepgram && status.xai) {
    await c.evaluate(`__TAURI__.core.invoke('audio_ready',{greeting:true})`);
    await new Promise(resolve => setTimeout(resolve, 12000));
    console.log('Real greeting outcome:', await c.evaluate(`__TAURI__.core.invoke('diagnostics_snapshot')`));
  }
} finally { c.close(); }
