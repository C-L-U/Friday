use crate::{app_state::InputEvent, config::Stt};
use anyhow::{bail, Context};
use async_trait::async_trait;
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};
use tokio::sync::mpsc;
use tokio_tungstenite::{
    connect_async_tls_with_config,
    tungstenite::{client::IntoClientRequest, Message},
};
use tokio_util::sync::CancellationToken;

pub struct Session {
    pub config: Stt,
    pub key: zeroize::Zeroizing<String>,
    pub audio: mpsc::Receiver<Bytes>,
    pub events: mpsc::Sender<InputEvent>,
    pub cancel: CancellationToken,
    pub tls: std::sync::Arc<rustls::ClientConfig>,
    pub last_voice: std::sync::Arc<parking_lot::RwLock<Instant>>,
}
#[async_trait]
pub trait SttBackend: Send + Sync {
    async fn run(&self, session: Session) -> anyhow::Result<()>;
}
pub struct DeepgramStt;
pub struct MockStt;

#[derive(Default)]
pub struct TranscriptAssembler {
    segments: String,
    last_end: f64,
}
impl TranscriptAssembler {
    pub fn accept(&mut self, value: &Value) -> Option<String> {
        if value["type"] == "Results" {
            let text = value["channel"]["alternatives"][0]["transcript"]
                .as_str()
                .unwrap_or("")
                .trim();
            let end =
                value["start"].as_f64().unwrap_or(0.0) + value["duration"].as_f64().unwrap_or(0.0);
            if value["is_final"] == true && !text.is_empty() && (end == 0.0 || end > self.last_end)
            {
                if !self.segments.is_empty() {
                    self.segments.push(' ');
                }
                self.segments.push_str(text);
                self.last_end = end;
            }
        }
        if (value["speech_final"] == true || value["type"] == "UtteranceEnd")
            && !self.segments.is_empty()
        {
            let result = std::mem::take(&mut self.segments);
            return (result.chars().filter(|c| c.is_alphanumeric()).count() >= 2).then_some(result);
        }
        None
    }
}

pub fn listen_url(config: &Stt) -> anyhow::Result<reqwest::Url> {
    let mut url = reqwest::Url::parse(&config.url)?;
    url.query_pairs_mut()
        .extend_pairs([
            ("model", config.model.as_str()),
            ("language", config.language.as_str()),
            ("encoding", "linear16"),
            ("sample_rate", "16000"),
            ("channels", "1"),
            ("interim_results", "true"),
            ("vad_events", "true"),
            ("smart_format", "true"),
            ("punctuate", "true"),
        ])
        .append_pair("endpointing", &config.endpointing_ms.to_string())
        .append_pair("utterance_end_ms", &config.utterance_end_ms.to_string());
    for keyterm in &config.keyterms {
        url.query_pairs_mut().append_pair("keyterm", keyterm);
    }
    Ok(url)
}

#[async_trait]
impl SttBackend for DeepgramStt {
    async fn run(&self, mut session: Session) -> anyhow::Result<()> {
        let mut pending = VecDeque::<Bytes>::new();
        let mut replay = VecDeque::<Bytes>::new();
        let mut attempts = 0u32;
        let mut transcript = TranscriptAssembler::default();
        loop {
            if session.cancel.is_cancelled() {
                return Ok(());
            }
            if attempts > 0 {
                let delay = Duration::from_millis(
                    (200 * 2u64.pow(attempts.min(5))).min(5000) + fastrand::u64(0..200),
                );
                let until = tokio::time::sleep(delay);
                tokio::pin!(until);
                loop {
                    tokio::select! {
                        _ = session.cancel.cancelled() => return Ok(()),
                        _ = &mut until => break,
                        frame = session.audio.recv() => match frame { Some(frame) => pending.push_back(frame), None => return Ok(()) },
                    }
                    if pending.len() > 1500 {
                        bail!("STT reconnect audio buffer exceeded 30 seconds");
                    }
                }
            }
            let mut request = listen_url(&session.config)?
                .as_str()
                .into_client_request()?;
            request.headers_mut().insert(
                "Authorization",
                crate::secrets::deepgram_header(&session.key)?,
            );
            let connecting = tokio::time::timeout(
                Duration::from_secs(5),
                connect_async_tls_with_config(
                    request,
                    None,
                    true,
                    Some(tokio_tungstenite::Connector::Rustls(session.tls.clone())),
                ),
            );
            crate::diagnostics::record("stt", "connecting", None);
            tokio::pin!(connecting);
            let connected = loop {
                tokio::select! {
                    _ = session.cancel.cancelled() => return Ok(()),
                    result = &mut connecting => break result,
                    frame = session.audio.recv() => match frame { Some(frame) => pending.push_back(frame), None => return Ok(()) },
                }
                if pending.len() > 1500 {
                    bail!("STT connection audio buffer exceeded 30 seconds");
                }
            };
            let mut socket = match connected {
                Ok(Ok((socket, _))) => socket,
                error => {
                    crate::diagnostics::record("stt", &format!("connect failed: {error:?}"), None);
                    attempts += 1;
                    if attempts >= 2 {
                        bail!("STT connect failed after one retry: {error:?}");
                    }
                    tracing::warn!(attempts, "STT connection unavailable; retrying");
                    continue;
                }
            };
            crate::diagnostics::record("stt", "open", None);
            // Provider timestamps restart with every connection.
            transcript.last_end = 0.0;
            while let Some(frame) = replay.pop_back() {
                pending.push_front(frame);
            }
            let mut last_send = Instant::now();
            let mut keepalive = tokio::time::interval(Duration::from_secs(5));
            let disconnected = loop {
                if let Some(frame) = pending.front().cloned() {
                    let sent = tokio::select! {
                        _ = session.cancel.cancelled() => break false,
                        result = tokio::time::timeout(Duration::from_secs(3),socket.send(Message::Binary(frame.clone()))) => result,
                    };
                    if !matches!(sent, Ok(Ok(()))) {
                        break true;
                    }
                    pending.pop_front();
                    replay.push_back(frame);
                    if replay.len() > 25 {
                        replay.pop_front();
                    }
                    last_send = Instant::now();
                }
                tokio::select! {
                    biased;
                    _ = session.cancel.cancelled() => break false,
                    message = socket.next() => match message {
                        Some(Ok(Message::Text(text))) => {
                            let value: Value = serde_json::from_str(&text).context("invalid STT event")?;
                            if value["type"] == "Results" { crate::diagnostics::record(if value["is_final"] == true { "transcript" } else { "interim" }, value["channel"]["alternatives"][0]["transcript"].as_str().unwrap_or(""), None); }
                            if value["type"] == "SpeechStarted" { let _ = session.events.send(InputEvent::RemoteSpeechStarted).await; }
                            if let Some(text) = transcript.accept(&value) {
                                let speech_end = *session.last_voice.read();
                                let _ = session.events.send(InputEvent::Transcript { text, speech_end }).await;
                            }
                            if value["type"] == "Error" { bail!("STT provider error: {}", crate::diagnostics::redact(&value.to_string())); }
                        }
                        Some(Ok(Message::Ping(data))) => {
                            let result = tokio::select! { _=session.cancel.cancelled()=>break false, result=tokio::time::timeout(Duration::from_secs(3),socket.send(Message::Pong(data)))=>result };
                            if !matches!(result,Ok(Ok(()))) { break true; }
                        }
                        Some(Ok(Message::Close(reason))) => { crate::diagnostics::record("stt", &format!("closed: {reason:?}"), None); break true; },
                        Some(Err(error)) => { crate::diagnostics::record("stt", &format!("socket error: {error}"), None); break true; },
                        None => { crate::diagnostics::record("stt", "unexpected EOF", None); break true; },
                        _ => {}
                    },
                    frame = session.audio.recv() => match frame { Some(frame) => pending.push_back(frame), None => break false },
                    _ = keepalive.tick() => {
                        if last_send.elapsed() >= Duration::from_secs(4) {
                            let result=tokio::select! { _=session.cancel.cancelled()=>break false, result=tokio::time::timeout(Duration::from_secs(3),socket.send(Message::Text(r#"{"type":"KeepAlive"}"#.into())))=>result };
                            if !matches!(result,Ok(Ok(()))) { break true; }
                            last_send = Instant::now();
                        }
                    },
                    _ = tokio::task::yield_now(), if !pending.is_empty() => {},
                }
            };
            if !disconnected {
                crate::diagnostics::record("stt", "closed: idle or cancelled", None);
                let _ = tokio::time::timeout(
                    Duration::from_millis(250),
                    socket.send(Message::Text(r#"{"type":"CloseStream"}"#.into())),
                )
                .await;
                return Ok(());
            }
            attempts += 1;
            if attempts >= 2 {
                bail!("STT socket disconnected after one retry");
            }
            crate::diagnostics::record("stt", "disconnected; reconnecting", None);
            tracing::warn!("STT disconnected; replaying pre-roll on reconnect");
        }
    }
}

#[async_trait]
impl SttBackend for MockStt {
    async fn run(&self, mut session: Session) -> anyhow::Result<()> {
        let mut vad = crate::audio_in::Vad::default();
        let mut heard = false;
        let mut silent_frames = 0;
        loop {
            tokio::select! {
                _ = session.cancel.cancelled() => return Ok(()),
                frame = session.audio.recv() => {
                    let Some(frame) = frame else { return Ok(()); };
                    if vad.process(&frame,0.012) { heard = true; silent_frames = 0; } else { silent_frames += 1; }
                    if heard && silent_frames >= 15 {
                        heard = false;
                        session.events.send(InputEvent::Transcript { text: "Jarvis, hola".into(), speech_end: Instant::now() }).await?;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn production_websocket_audio_transcript_and_graceful_cancel() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut config = crate::config::Config::parse("").unwrap().stt;
        config.url = format!("ws://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            tokio::time::timeout(Duration::from_secs(5), async move {
                let (tcp, _) = listener.accept().await.unwrap();
                let mut socket = tokio_tungstenite::accept_async(tcp).await.unwrap();
                for index in 0..25 {
                    // Ignore a timer-driven KeepAlive if it races the queued audio.
                    loop {
                        let message = socket.next().await.unwrap().unwrap();
                        if let Message::Binary(data) = message {
                            assert_eq!(&data[..], &[index; crate::audio_in::FRAME_BYTES]);
                            break;
                        }
                    }
                }
                for event in [
                    json!({"type":"SpeechStarted"}),
                    json!({"type":"Results","is_final":false,"channel":{"alternatives":[{"transcript":"No debe llegar"}]}}),
                    json!({"type":"Results","is_final":true,"start":0.0,"duration":0.2,"channel":{"alternatives":[{"transcript":"Jarvis,"}]}}),
                    json!({"type":"Results","is_final":true,"speech_final":true,"start":0.2,"duration":0.3,"channel":{"alternatives":[{"transcript":"¿qué tal?"}]}}),
                ] {
                    socket.send(Message::Text(event.to_string().into())).await.unwrap();
                }
                loop {
                    let message = socket.next().await.unwrap().unwrap();
                    if let Message::Text(text) = message {
                        let event: Value = serde_json::from_str(&text).unwrap();
                        if event["type"] == "CloseStream" { break; }
                    }
                }
            }).await.unwrap();
        });
        let (tx, rx) = mpsc::channel(32);
        for index in 0..25 {
            tx.send(Bytes::from(vec![index; crate::audio_in::FRAME_BYTES]))
                .await
                .unwrap();
        }
        let (events, mut received) = mpsc::channel(16);
        let cancel = CancellationToken::new();
        let speech_end = Instant::now() - Duration::from_millis(300);
        let session = Session {
            config,
            key: zeroize::Zeroizing::new("fixture-key".into()),
            audio: rx,
            events,
            cancel: cancel.clone(),
            tls: std::sync::Arc::new(
                rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
                    rustls::crypto::ring::default_provider(),
                ))
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_root_certificates(rustls::RootCertStore::empty())
                .with_no_client_auth(),
            ),
            last_voice: std::sync::Arc::new(parking_lot::RwLock::new(speech_end)),
        };
        let task = tokio::spawn(async move { DeepgramStt.run(session).await });
        let next = tokio::time::timeout(Duration::from_secs(2), received.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(next, InputEvent::RemoteSpeechStarted));
        let next = tokio::time::timeout(Duration::from_secs(2), received.recv())
            .await
            .unwrap()
            .unwrap();
        match next {
            InputEvent::Transcript {
                text,
                speech_end: actual,
            } => {
                assert_eq!(text, "Jarvis, ¿qué tal?");
                assert_eq!(actual, speech_end);
            }
            _ => panic!("expected final transcript"),
        }
        cancel.cancel();
        tokio::time::timeout(Duration::from_millis(500), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        server.await.unwrap();
    }
    #[test]
    fn joins_final_segments() {
        let mut a = TranscriptAssembler::default();
        assert!(a.accept(&json!({"type":"Results","is_final":true,"channel":{"alternatives":[{"transcript":"Hola"}]}})).is_none());
        assert_eq!(a.accept(&json!({"type":"Results","is_final":true,"speech_final":true,"channel":{"alternatives":[{"transcript":"Jarvis"}]}})),Some("Hola Jarvis".into()));
        assert!(a.accept(&json!({"type":"UtteranceEnd"})).is_none());
    }
    #[test]
    fn interim_not_committed() {
        let mut a = TranscriptAssembler::default();
        a.accept(&json!({"type":"Results","is_final":false,"channel":{"alternatives":[{"transcript":"wrong"}]}}));
        assert!(a.accept(&json!({"type":"UtteranceEnd"})).is_none());
    }
    #[test]
    fn correct_query() {
        let c = crate::config::Config::parse("").unwrap();
        let url = listen_url(&c.stt).unwrap();
        assert!(url.as_str().contains("interim_results=true"));
        assert_eq!(url.query_pairs().filter(|(k, _)| k == "keyterm").count(), 3);
    }
}
