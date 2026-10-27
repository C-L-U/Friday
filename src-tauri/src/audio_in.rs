use crate::{
    app_state::{InputEvent, Phase, Shared},
    stt::{DeepgramStt, MockStt, Session, SttBackend},
};
use bytes::Bytes;
use std::{
    collections::VecDeque,
    sync::atomic::Ordering,
    time::{Duration, Instant},
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub const FRAME_BYTES: usize = 640;
const PREROLL_FRAMES: usize = 25;

#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[error("Se requiere un frame PCM binario de 20 ms (640 bytes)")]
    InvalidFrame,
    #[error("La cola de audio está ocupada")]
    Backpressure,
}

/// Adaptive energy gate. Noise floor learns only below the voice threshold; the
/// sustained counter is deliberately reset by silence to reject impulsive noise.
pub struct Vad {
    noise: f32,
    pub sustained_ms: u32,
}
impl Default for Vad {
    fn default() -> Self {
        Self {
            noise: 0.002,
            sustained_ms: 0,
        }
    }
}
impl Vad {
    pub fn process(&mut self, pcm: &[u8], threshold: f32) -> bool {
        let count = pcm.len() / 2;
        if count == 0 {
            return false;
        }
        let mut energy = 0.0;
        let mut mean = 0.0;
        for pair in pcm.as_chunks::<2>().0 {
            let sample = i16::from_le_bytes([pair[0], pair[1]]) as f32 / 32768.0;
            energy += sample * sample;
            mean += sample;
        }
        mean /= count as f32;
        let rms = (energy / count as f32 - mean * mean).max(0.0).sqrt();
        let voiced = rms >= threshold.max(self.noise * 3.0);
        if voiced {
            self.sustained_ms = self.sustained_ms.saturating_add(20);
        } else {
            self.noise = self.noise * 0.98 + rms * 0.02;
            self.sustained_ms = 0;
        }
        voiced
    }
}

pub fn enqueue(state: &Shared, bytes: &[u8]) -> Result<(), AudioError> {
    if bytes.len() != FRAME_BYTES {
        return Err(AudioError::InvalidFrame);
    }
    if state.muted.load(Ordering::Relaxed) {
        return Ok(());
    }
    state
        .audio_tx
        .try_send(Bytes::copy_from_slice(bytes))
        .map_err(|_| AudioError::Backpressure)
}

pub async fn run(
    state: Shared,
    app: tauri::AppHandle,
    mut input: mpsc::Receiver<Bytes>,
    cancel: CancellationToken,
) {
    let mut vad = Vad::default();
    let mut preroll = VecDeque::with_capacity(PREROLL_FRAMES);
    let mut session: Option<(mpsc::Sender<Bytes>, CancellationToken)> = None;
    let mut last_voice = Instant::now();
    let mut voice_sent = false;
    let mut barge_sent = false;
    let mut frames = 0u64;
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    loop {
        let frame = tokio::select! {
            _ = cancel.cancelled() => break,
            frame = input.recv() => match frame { Some(frame) => Some(frame), None => break },
            _ = tick.tick() => None,
        };
        let config = state.config.current.read().clone();
        let muted = state.muted.load(Ordering::Relaxed);
        if muted || last_voice.elapsed() >= Duration::from_secs(config.stt.idle_close_secs) {
            if let Some((_, token)) = session.take() {
                token.cancel();
            }
            if state.phase() == Phase::Listening {
                state.set_phase(&app, Phase::Idle);
            }
        }
        if muted {
            preroll.clear();
            vad = Vad::default();
            voice_sent = false;
            barge_sent = false;
            continue;
        }
        let Some(frame) = frame else {
            continue;
        };
        let voiced = vad.process(&frame, config.activation.vad_threshold);
        frames += 1;
        if frames % 50 == 1 {
            crate::diagnostics::record("audio_frames", &frames.to_string(), None);
        }
        preroll.push_back(frame.clone());
        if preroll.len() > PREROLL_FRAMES {
            preroll.pop_front();
        }
        if voiced {
            last_voice = Instant::now();
            *state.last_voice.write() = last_voice;
            if vad.sustained_ms >= 60 && !voice_sent {
                voice_sent = true;
                crate::diagnostics::record("vad", "voice start", None);
                let warming = state.clone();
                crate::diagnostics::spawn("audio_in", async move {
                    warming.prewarm().await;
                });
                let _ = state.event_tx.send(InputEvent::SpeechStarted).await;
            }
            if vad.sustained_ms >= config.activation.barge_in_ms && !barge_sent {
                barge_sent = true;
                let _ = state.event_tx.send(InputEvent::SustainedVoice).await;
            }
        } else {
            if voice_sent {
                crate::diagnostics::record("vad", "voice end", None);
            }
            voice_sent = false;
            barge_sent = false;
        }
        if session.as_ref().is_some_and(|(tx, _)| tx.is_closed()) {
            session = None;
        }
        if session.is_none()
            && vad.sustained_ms >= 60
            && (state.mock || !state.keys.read().deepgram.is_empty())
            && state
                .stt_retry_after
                .read()
                .is_none_or(|until| Instant::now() >= until)
        {
            let (tx, rx) = mpsc::channel(1500);
            for old in &preroll {
                let _ = tx.try_send(old.clone());
            }
            let token = cancel.child_token();
            let backend: Box<dyn SttBackend> = if state.mock {
                Box::new(MockStt)
            } else {
                Box::new(DeepgramStt)
            };
            let request = Session {
                config: config.stt.clone(),
                key: zeroize::Zeroizing::new(state.keys.read().deepgram.clone()),
                audio: rx,
                events: state.event_tx.clone(),
                cancel: token.clone(),
                tls: state.websocket_tls.clone(),
                last_voice: state.last_voice.clone(),
            };
            let shared = state.clone();
            let handle = app.clone();
            crate::diagnostics::spawn("audio_in", async move {
                if let Err(error) = crate::diagnostics::checked("stt", backend.run(request)).await {
                    *shared.stt_retry_after.write() =
                        Some(Instant::now() + Duration::from_secs(15));
                    crate::diagnostics::error("stt", &format!("{error:#}"), None);
                    use tauri::Emitter;
                    let _ = handle.emit(
                        "assistant://local_error",
                        "No pude escuchar la voz. Revisa el diagnóstico.",
                    );
                }
            });
            session = Some((tx, token));
            if state.phase() == Phase::Idle {
                state.set_phase(&app, Phase::Listening);
            }
            continue;
        }
        // Supply the complete speech and a generous silence tail for server endpointing.
        if let Some((tx, _)) = &session {
            if last_voice.elapsed() < Duration::from_millis(1500) && tx.try_send(frame).is_err() {
                crate::diagnostics::error(
                    "stt_backlog",
                    "STT backlog exceeded 30 seconds; restarting with pre-roll",
                    None,
                );
                if let Some((_, token)) = session.take() {
                    token.cancel();
                }
            }
        }
    }
    if let Some((_, token)) = session {
        token.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tone(amplitude: f32) -> Vec<u8> {
        (0..320)
            .flat_map(|i| ((i as f32 * 0.1).sin().mul_add(amplitude, 0.0) as i16).to_le_bytes())
            .collect()
    }
    #[test]
    fn silence_is_not_voice() {
        let mut v = Vad::default();
        for _ in 0..1000 {
            assert!(!v.process(&[0; 640], 0.012));
        }
    }
    #[test]
    fn rejects_constant_dc() {
        assert!(!Vad::default().process(&[40; 640], 0.012));
    }
    #[test]
    fn sustained_speech() {
        let mut v = Vad::default();
        for _ in 0..10 {
            assert!(v.process(&tone(2000.0), 0.012));
        }
        assert_eq!(v.sustained_ms, 200);
    }
    #[test]
    fn impulse_does_not_barge() {
        let mut v = Vad::default();
        v.process(&tone(4000.0), 0.012);
        v.process(&[0; 640], 0.012);
        assert_eq!(v.sustained_ms, 0);
    }
    #[test]
    fn low_noise_is_ignored() {
        let mut v = Vad::default();
        for _ in 0..500 {
            assert!(!v.process(&tone(100.0), 0.012));
        }
    }
}
