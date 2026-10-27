import { PlaybackScheduler } from './playback.js';
const { listen } = window.__TAURI__.event;
const { invoke } = window.__TAURI__.core;
const { getCurrentWindow } = window.__TAURI__.window;
const { LogicalSize } = window.__TAURI__.dpi;
const $ = selector => document.querySelector(selector);
let expanded = false;
let settings;
let promptTimer;
let promptWrite = Promise.resolve();
let resizing = Promise.resolve();
let audioContext, micStream, capture, micAnalyser;
let audioStarting = false;
let micMuted = false;
let inflightFrames = 0;
let droppedFrames = 0;
let playback, audioChannel;
function diagnostic(stage, detail, turn = null) { return invoke('frontend_diagnostic', { stage, detail: String(detail), turn }).catch(console.error); }
window.addEventListener('error', event => diagnostic('frontend_error', event.message));
window.addEventListener('unhandledrejection', event => diagnostic('frontend_error', event.reason));
function renderHealth(fields) {
  for (const [stage, detail] of Object.entries(fields)) {
    let row = document.getElementById('health-' + stage);
    if (!row) { const label = document.createElement('dt'); label.textContent = stage; row = document.createElement('dd'); row.id = 'health-' + stage; $('#health').append(label, row); }
    row.textContent = detail;
  }
}
await listen('diagnostics://update', ({ payload }) => {
  renderHealth({ [payload.stage]: payload.detail });
  if (payload.stage === 'last_error') $('#test-status').textContent = payload.detail;
  if (payload.stage === 'transcript') $('#test-status').textContent = `He oído: ${payload.detail}`;
  if (payload.stage === 'latency') refreshLatency();
});
async function refreshLatency() {
  const lines = await invoke('debug_latency');
  $('#latency').textContent = lines.slice(-10).map(r => `${r.turn} ${r.outcome}: ${JSON.stringify(r.milliseconds_from_speech_end)}`).join('\n');
}
renderHealth(await invoke('diagnostics_snapshot'));
let source, silent, workletLoaded = false, micStarting = false, devicesListed = false;
function contextState() {
  return `${audioContext?.state || 'no creado'} · ${audioContext?.state === 'running' ? 'desbloqueado' : 'bloqueado: pulsa el orbe o Test voice'}`;
}
function unlockAudio() {
  if (!audioContext) return;
  diagnostic('audio_context', contextState());
  audioContext.resume().then(() => diagnostic('audio_context', contextState())).catch(showError);
}
async function listDevices() {
  const devices = await navigator.mediaDevices.enumerateDevices();
  for (const [id, kind, configured] of [['#input-device', 'audioinput', settings.config.ui.input_device], ['#output-device', 'audiooutput', settings.config.ui.output_device]]) {
    const select = $(id), selected = devicesListed ? select.value : (configured || '');
    select.replaceChildren(new Option('Predeterminado del sistema', ''));
    for (const device of devices.filter(d => d.kind === kind && d.deviceId !== 'default')) select.add(new Option(device.label || 'Dispositivo sin permiso', device.deviceId));
    if (selected && ![...select.options].some(o => o.value === selected)) select.add(new Option('Dispositivo guardado no disponible', selected));
    select.value = selected;
  }
  devicesListed = true;
  return devices;
}
async function applyOutput() {
  const id = $('#output-device').value;
  if (typeof audioContext.setSinkId === 'function') await audioContext.setSinkId(id);
  else if (id) throw Error('Este WebView2 no permite seleccionar salida. Cambia la salida predeterminada en Windows.');
  $('#output-device').disabled = typeof audioContext.setSinkId !== 'function';
  const devices = await navigator.mediaDevices.enumerateDevices();
  const device = devices.find(d => d.kind === 'audiooutput' && d.deviceId === (id || 'default'));
  diagnostic('output_device', `${device?.label || 'Predeterminado del sistema'} · ${id ? 'seleccionado' : 'predeterminado'} · setSinkId ${typeof audioContext.setSinkId}`);
}
async function startMicrophone() {
  if (micStarting) return;
  micStarting = true;
  try {
    micStream?.getTracks().forEach(track => track.stop());
    source?.disconnect(); capture?.disconnect(); silent?.disconnect(); micAnalyser?.disconnect();
    const deviceId = $('#input-device').value;
    diagnostic('mic_permission', 'solicitando getUserMedia');
    micStream = await navigator.mediaDevices.getUserMedia({ audio: { ...(deviceId ? { deviceId: { exact: deviceId } } : {}), channelCount: 1, echoCancellation: true, noiseSuppression: true, autoGainControl: true }, video: false });
    const track = micStream.getAudioTracks()[0];
    track.enabled = !micMuted;
    track.onended = () => showError('El micrófono se desconectó. Selecciona un dispositivo y pulsa Test microphone.');
    diagnostic('mic_permission', 'granted: getUserMedia succeeded'); $('#mic-hint').hidden = true;
    diagnostic('input_device', track.label || 'Predeterminado del sistema');
    if (!workletLoaded) { await audioContext.audioWorklet.addModule('./capture-worklet.js'); workletLoaded = true; }
    source = audioContext.createMediaStreamSource(micStream);
    micAnalyser = audioContext.createAnalyser(); micAnalyser.fftSize = 256;
    capture = new AudioWorkletNode(audioContext, 'jarvis-capture', { channelCount: 1, numberOfInputs: 1, numberOfOutputs: 1, outputChannelCount: [1] });
    silent = audioContext.createGain(); silent.gain.value = 0;
    source.connect(micAnalyser); micAnalyser.connect(capture); capture.connect(silent).connect(audioContext.destination);
    capture.port.onmessage = ({ data }) => {
      if (micMuted) return;
      if (inflightFrames >= 8) { if (++droppedFrames % 50 === 1) diagnostic('frontend_error', 'Sobrecarga de audio IPC'); return; }
      inflightFrames++;
      invoke('audio_frame', data).catch(error => { if (++droppedFrames % 50 === 1) showError(error); }).finally(() => { inflightFrames--; });
    };
    await listDevices(); await applyOutput().catch(showError);
  } catch (error) {
    diagnostic('mic_permission', `${error.name}: ${error.message}`);
    $('#mic-hint').hidden = error.name !== 'NotAllowedError';
    throw error;
  } finally { micStarting = false; }
}
async function startAudio({ mock, firstRun, microphone = true }) {
  if (audioStarting) return;
  audioStarting = true;
  try {
    if (!audioContext) {
      audioContext = new AudioContext({ latencyHint: 'interactive' });
      audioContext.onstatechange = () => diagnostic('audio_context', contextState());
      unlockAudio(); // Do not await a gesture-blocked resume and strand the Channel.
      let reports = Promise.resolve();
      playback = new PlaybackScheduler(audioContext, event => {
        if (event.kind === 'started') $('#test-status').textContent = 'Reproducción iniciada en la salida indicada.';
        reports = reports.then(() => invoke('playback_event', event)).catch(showError);
      });
      audioChannel = new window.__TAURI__.core.Channel();
      audioChannel.onmessage = packet => { try { unlockAudio(); playback.accept(packet); } catch (error) { showError(error); } };
      await invoke('connect_audio', { onAudio: audioChannel });
      await listDevices(); await applyOutput().catch(showError);
      const samples = new Float32Array(256);
      function animate() {
        if (!micMuted && micAnalyser) {
          micAnalyser.getFloatTimeDomainData(samples);
          const rms = Math.sqrt(samples.reduce((sum, value) => sum + value * value, 0) / samples.length);
          $('#input-level').value = Math.min(1, rms * 8);
          if (!document.body.classList.contains('speaking')) document.body.style.setProperty('--level', Math.min(1, rms * 8));
        } else $('#input-level').value = 0;
        if (document.body.classList.contains('speaking')) {
          playback.analyser.getFloatTimeDomainData(samples);
          const rms = Math.sqrt(samples.reduce((sum, value) => sum + value * value, 0) / samples.length);
          document.body.style.setProperty('--level', Math.min(1, rms * 8));
        }
        requestAnimationFrame(animate);
      }
      requestAnimationFrame(animate);
    }
    if (!mock && microphone && !micStream?.getAudioTracks().some(t => t.readyState === 'live')) await startMicrophone();
    if (microphone || firstRun) await invoke('audio_ready', { greeting: firstRun });
  } catch (error) { showError(error); }
  finally { audioStarting = false; }
}
window.addEventListener('jarvis-ready', event => startAudio(event.detail));
export function showError(error) { $('#status').textContent = String(error); diagnostic('frontend_error', error); setExpanded(true); }
function setExpanded(open) {
  expanded = open;
  if (open) { refreshSecrets().catch(error => { $('#status').textContent = String(error); }); refreshLatency().catch(console.error); }
  $('#gear').setAttribute('aria-expanded', String(open));
  resizing = resizing.then(async () => {
    if (open) await getCurrentWindow().show();
    const width = open ? Math.min(440, screen.availWidth) : 160;
    const height = open ? Math.min(740, screen.availHeight - 40) : 160;
    const fromW = innerWidth, fromH = innerHeight;
    $('#settings').hidden = true;
    if (matchMedia('(prefers-reduced-motion: reduce)').matches) await getCurrentWindow().setSize(new LogicalSize(width, height));
    else {
      const started = performance.now();
      let t = 0;
      while (t < 1) {
        t = Math.min(1, (performance.now() - started) / 160);
        const ease = 1 - (1 - t) ** 3;
        await getCurrentWindow().setSize(new LogicalSize(Math.round(fromW + (width-fromW)*ease), Math.round(fromH + (height-fromH)*ease)));
        if (t < 1) await new Promise(requestAnimationFrame);
      }
    }
    document.body.classList.toggle('expanded', open); $('#settings').hidden = !open;
  }).catch(error => { $('#status').textContent = String(error); });
}
$('#gear').addEventListener('click', () => setExpanded(!expanded));
document.addEventListener('keydown', event => {
  if (event.key === 'Escape') setExpanded(false);
  if (event.key === 'Enter' && (event.ctrlKey || event.metaKey) && expanded) { event.preventDefault(); $('#form').requestSubmit(); }
});
for (const eye of document.querySelectorAll('.eye')) {
  eye.addEventListener('click', () => { const input = document.getElementById(eye.dataset.input); input.type = input.type === 'password' ? 'text' : 'password'; eye.setAttribute('aria-pressed', String(input.type === 'text')); });
}
function savePrompt() {
  clearTimeout(promptTimer);
  const prompt = $('#prompt').value;
  promptWrite = promptWrite.catch(() => {}).then(() => invoke('save_prompt', { prompt }));
  return promptWrite;
}
$('#prompt').addEventListener('input', () => { clearTimeout(promptTimer); promptTimer = setTimeout(() => savePrompt().catch(showError), 600); });
$('#restore').addEventListener('click', async event => { event.preventDefault(); clearTimeout(promptTimer); try { await promptWrite; $('#prompt').value = await invoke('restore_prompt'); } catch (error) { showError(error); } });
function dot(id, ok) { const el = $(id); el.className = `dot ${ok ? 'good' : 'bad'}`; el.setAttribute('aria-label', ok ? 'Clave validada' : 'No se pudo validar'); }
async function refreshSecrets() {
  const status = await invoke('secrets_status');
  for (const name of ['deepgram', 'xai']) {
    const saved = status[name] === 'present', input = $('#' + name);
    input.placeholder = saved ? '••••••••••••  guardada' : 'Introduce tu clave';
    if (!input.value) input.readOnly = saved;
    $(`.change[data-input="${name}"]`).hidden = !saved;
  }
}
function validation(result) {
  dot('#dg-dot', result.deepgram); dot('#xai-dot', result.xai);
  $('#deepgram-message').textContent = result.deepgram_message;
  $('#xai-message').textContent = result.xai_message;
  $('#available-models').replaceChildren(new Option('Selecciona un modelo', ''), ...result.models.map(id => new Option(id, id)));
  $('#available-models').value = $('#llm-model').value;
}
await listen('settings://validation', ({ payload }) => validation(payload));
$('#available-models').addEventListener('change', () => { if ($('#available-models').value) $('#llm-model').value = $('#available-models').value; });
for (const button of document.querySelectorAll('.change')) button.addEventListener('click', () => { const input = $('#' + button.dataset.input); input.readOnly = false; input.focus(); });
$('#form').addEventListener('submit', async event => {
  event.preventDefault(); $('#save').disabled = true; $('#status').textContent = 'Validando…';
  try {
    await savePrompt();
    const config = structuredClone(settings.config);
    config.llm.model = $('#llm-model').value.trim(); config.tts.model = $('#tts-model').value.trim();
    config.tts.speed = Number($('#speed').value); config.stt.language = $('#language').value;
    config.activation.require_wake_word = $('#wake-enabled').checked; config.activation.wake_word = $('#wake-word').value.trim();
    config.coding.default_agent = $('#agent').value; config.coding.projects_dir = $('#projects').value.trim();
    config.ui.input_device = $('#input-device').value; config.ui.output_device = $('#output-device').value;
    const pending = invoke('save_settings', { config, deepgram: $('#deepgram').value, xai: $('#xai').value });
    $('#deepgram').value = ''; $('#xai').value = '';
    const result = await pending;
    validation(result); await refreshSecrets();
    settings.config = config;
    if (result.deepgram && result.xai) {
      settings.config = config; settings.deepgram_saved = true; settings.xai_saved = true;
      $('#status').textContent = result.model_listed ? 'Claves guardadas y validadas.' : 'Claves guardadas. Elige un modelo disponible en Avanzado.';
      document.body.classList.remove('attention');
      window.dispatchEvent(new CustomEvent('jarvis-ready', { detail: { firstRun: true, mock: settings.mock } }));
    } else { $('#status').textContent = 'Guardadas; validación pendiente o fallida. Revisa el mensaje de cada proveedor.'; }
  } catch (error) { showError(error); }
  finally { $('#save').disabled = false; }
});
document.querySelector('#orb').addEventListener('pointerdown', () => { if (!audioContext && settings) startAudio({ mock: settings.mock, firstRun: false }); else unlockAudio(); });
document.querySelector('#orb').addEventListener('dblclick', () => invoke('toggle_window'));
$('#mic-settings').addEventListener('click', () => invoke('open_microphone_settings').catch(showError));
$('#test-voice').addEventListener('click', async () => {
  try {
    unlockAudio(); await startAudio({ mock: settings.mock, firstRun: false, microphone: false }); unlockAudio();
    $('#test-status').textContent = 'Solicitando voz…'; await invoke('test_voice');
  } catch (error) { showError(error); }
});
let micTestTimer;
$('#test-mic').addEventListener('click', async () => {
  try {
    unlockAudio(); await startAudio({ mock: settings.mock, firstRun: false, microphone: false });
    if (!settings.mock) await startMicrophone();
    await invoke('test_microphone', { enabled: true });
    $('#test-status').textContent = 'Habla ahora: prueba de transcripción durante 20 segundos.';
    clearTimeout(micTestTimer); micTestTimer = setTimeout(() => invoke('test_microphone', { enabled: false }).catch(showError), 20000);
  } catch (error) { showError(error); }
});
for (const id of ['#input-device', '#output-device']) $(id).addEventListener('change', async () => {
  try {
    if (audioContext) { if (id === '#input-device' && !settings.mock) await startMicrophone(); else await applyOutput(); }
    await invoke('save_audio_devices', { input: $('#input-device').value, output: $('#output-device').value });
    settings.config.ui.input_device = $('#input-device').value; settings.config.ui.output_device = $('#output-device').value;
  } catch (error) { showError(error); }
});
navigator.mediaDevices?.addEventListener('devicechange', () => listDevices().then(() => audioContext && applyOutput()).catch(showError));
let errorTimer, ignoredTimer;
await listen('state://error', () => { document.body.classList.add('error'); clearTimeout(errorTimer); errorTimer = setTimeout(() => document.body.classList.remove('error'), 4500); });
await listen('state://ignored', () => { document.body.classList.add('ignored'); clearTimeout(ignoredTimer); ignoredTimer = setTimeout(() => document.body.classList.remove('ignored'), 450); });
await listen('assistant://local_error', ({ payload }) => {
  const voice = window.speechSynthesis?.getVoices().find(v => v.localService && v.lang.startsWith('es'));
  if (!voice) { diagnostic('playback', 'No hay voz local española disponible para el aviso'); return; }
  const utterance = new SpeechSynthesisUtterance(payload); utterance.voice = voice;
  utterance.onerror = event => diagnostic('playback', `Aviso local: ${event.error}`);
  speechSynthesis.cancel(); speechSynthesis.speak(utterance);
});
await listen('mic://muted', ({ payload }) => { micMuted = payload; document.body.classList.toggle('muted', payload); micStream?.getAudioTracks().forEach(track => { track.enabled = !payload; }); });
await listen('stop_audio', ({ payload }) => { playback?.stop(payload); window.speechSynthesis?.cancel(); });
await listen('assistant://error', ({ payload }) => showError(payload));
for (const state of ['idle', 'listening', 'thinking', 'speaking']) {
  await listen(`state://${state}`, () => {
    document.body.classList.remove('idle', 'listening', 'thinking', 'speaking');
    document.body.classList.add(state);
  });
}
try {
  settings = await invoke('bootstrap');
  const c = settings.config;
  $('#prompt').value = settings.prompt; $('#llm-model').value = c.llm.model; $('#tts-model').value = c.tts.model;
  $('#speed').value = c.tts.speed; $('#language').value = c.stt.language; $('#wake-enabled').checked = c.activation.require_wake_word;
  $('#wake-word').value = c.activation.wake_word; $('#agent').value = c.coding.default_agent; $('#projects').value = c.coding.projects_dir;
  await refreshSecrets(); await listDevices();
  if (!(settings.deepgram_saved && settings.xai_saved) && !settings.mock) { document.body.classList.add('attention'); setExpanded(true); }
  else { window.dispatchEvent(new CustomEvent('jarvis-ready', { detail: { firstRun: false, mock: settings.mock } })); }
} catch (error) { showError(error); }
