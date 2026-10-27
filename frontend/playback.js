export class PlaybackScheduler {
  constructor(context, report) {
    this.context = context; this.report = report;
    this.analyser = context.createAnalyser(); this.analyser.fftSize = 256; this.analyser.connect(context.destination);
    this.sources = new Set(); this.timers = new Set(); this.turn = 0; this.stoppedThrough = 0;
    this.end = context.currentTime; this.started = false;
  }
  at(time, action) {
    const turn = this.turn;
    const tick = () => {
      if (turn !== this.turn || turn <= this.stoppedThrough) return;
      const remaining = time - this.context.currentTime;
      if (remaining > 0.002) { const timer = setTimeout(() => { this.timers.delete(timer); tick(); }, Math.max(4, remaining * 1000)); this.timers.add(timer); }
      else action();
    };
    tick();
  }
  accept(packet) {
    if (!(packet instanceof ArrayBuffer) || packet.byteLength < 12 || packet.byteLength % 2) throw new Error('Paquete PCM inválido');
    const view = new DataView(packet);
    const turn = view.getUint32(0, true), sentence = view.getUint32(4, true), flags = view.getUint32(8, true);
    if (turn <= this.stoppedThrough || turn < this.turn) return;
    if (turn > this.turn) { this.stop(this.turn); this.turn = turn; this.started = false; this.end = this.context.currentTime; }
    if (packet.byteLength > 12) {
      const samples = (packet.byteLength - 12) / 2;
      const buffer = this.context.createBuffer(1, samples, 24000);
      const data = buffer.getChannelData(0);
      for (let i = 0; i < samples; i++) data[i] = view.getInt16(12 + i * 2, true) / 32768;
      const source = this.context.createBufferSource(); source.buffer = buffer; source.connect(this.analyser);
      const start = Math.max(this.end, this.context.currentTime + 0.018);
      source.start(start); this.end = start + buffer.duration; this.sources.add(source);
      source.onended = () => { source.disconnect(); this.sources.delete(source); };
      if (!this.started) {
        this.started = true;
        this.at(start + (this.context.outputLatency || 0), () => this.report({ turn, sentence, kind: 'started' }));
      }
    }
    if (flags & 1) this.at(this.end + (this.context.outputLatency || 0), () => this.report({ turn, sentence, kind: 'sentence' }));
    if (flags & 2) this.at(this.end + (this.context.outputLatency || 0), () => this.report({ turn, sentence: 0, kind: 'ended' }));
  }
  stop(turn = this.turn) {
    this.stoppedThrough = Math.max(this.stoppedThrough, turn);
    if (turn < this.turn) return;
    for (const timer of this.timers) clearTimeout(timer); this.timers.clear();
    for (const source of this.sources) { try { source.stop(); } catch {} source.disconnect(); source.onended = null; }
    this.sources.clear(); this.end = this.context.currentTime;
  }
}
