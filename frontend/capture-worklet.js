// Polyphase windowed-sinc low-pass: supports 44.1/48/96 kHz input without
// resetting phase at render-quantum boundaries. Every packet is exactly 20 ms.
export class PcmResampler {
  constructor(rate, emit) {
    this.rate = rate; this.emit = emit; this.phase = 0; this.index = 0;
    this.ring = new Float32Array(64); this.frame = new Int16Array(320); this.used = 0;
    this.kernels = Array.from({ length: 32 }, (_, phase) => {
      const weights = new Float64Array(48); const cutoff = Math.min(0.45, 7200 / rate);
      let sum = 0;
      for (let i = 0; i < weights.length; i++) {
        const x = i - 23.5 - phase / 32;
        const sinc = Math.abs(x) < 1e-9 ? 2 * cutoff : Math.sin(2 * Math.PI * cutoff * x) / (Math.PI * x);
        weights[i] = sinc * (0.42 - 0.5 * Math.cos(2 * Math.PI * i / 47) + 0.08 * Math.cos(4 * Math.PI * i / 47));
        sum += weights[i];
      }
      return weights.map(value => value / sum);
    });
  }
  push(input) {
    for (const sample of input) {
      this.ring[this.index] = sample; this.index = (this.index + 1) & 63;
      this.phase += 16000;
      if (this.phase < this.rate) continue;
      this.phase -= this.rate;
      const kernel = this.kernels[Math.min(31, Math.floor(this.phase / 16000 * 32))];
      let filtered = 0;
      for (let i = 0; i < 48; i++) filtered += this.ring[(this.index - 1 - i + 128) & 63] * kernel[i];
      this.frame[this.used++] = Math.round(Math.max(-1, Math.min(1, filtered)) * 32767);
      if (this.used === 320) {
        const data = this.frame.buffer; this.emit(data); this.frame = new Int16Array(320); this.used = 0;
      }
    }
  }
}
class CaptureProcessor extends AudioWorkletProcessor {
  constructor() {
    super();
    this.resampler = new PcmResampler(sampleRate, data => this.port.postMessage(data, [data]));
  }
  process(inputs) { const mono = inputs[0]?.[0]; if (mono) this.resampler.push(mono); return true; }
}
registerProcessor('jarvis-capture', CaptureProcessor);
