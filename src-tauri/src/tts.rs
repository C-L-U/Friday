use crate::{
    app_state::{InputEvent, Shared},
    config::Tts,
};
use anyhow::{bail, Context};
use async_trait::async_trait;
use bytes::Bytes;
use futures_util::{future::BoxFuture, stream::FuturesUnordered, SinkExt, StreamExt};
use std::{collections::VecDeque, sync::Arc};
use tokio::sync::mpsc;
use tokio_tungstenite::{
    connect_async_with_config,
    tungstenite::{client::IntoClientRequest, Message},
};
use tokio_util::sync::CancellationToken;

#[async_trait]
pub trait TtsBackend: Send + Sync {
    async fn stream(
        &self,
        text: String,
        output: mpsc::Sender<Bytes>,
        cancel: CancellationToken,
    ) -> anyhow::Result<()>;
}
pub struct HttpTts {
    pub client: reqwest::Client,
    pub config: Tts,
    pub key: zeroize::Zeroizing<String>,
}
pub struct WebSocketTts {
    pub config: Tts,
    pub key: zeroize::Zeroizing<String>,
}
pub struct MockTts;

/// Retry only before delivering PCM: retrying a partial utterance duplicates speech.
pub async fn timed_stream(
    backend: &dyn TtsBackend,
    text: String,
    output: mpsc::Sender<Bytes>,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    for attempt in 0..2 {
        let (tx, mut rx) = mpsc::channel(16);
        let future = backend.stream(text.clone(), tx, cancel.clone());
        tokio::pin!(future);
        let deadline = tokio::time::sleep(std::time::Duration::from_secs(5));
        tokio::pin!(deadline);
        let mut delivered = false;
        let result = loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => return Ok(()),
                Some(bytes) = rx.recv() => {
                    if !bytes.is_empty() { delivered = true; send_pcm(&output, bytes, &cancel).await?; }
                },
                result = &mut future => break result,
                _ = &mut deadline, if !delivered => break Err(anyhow::anyhow!("TTS first byte timeout (5 s)")),
            }
        };
        while let Ok(bytes) = rx.try_recv() {
            if !bytes.is_empty() {
                delivered = true;
                send_pcm(&output, bytes, &cancel).await?;
            }
        }
        let result = result.and_then(|()| {
            if delivered {
                Ok(())
            } else {
                Err(anyhow::anyhow!("TTS returned empty audio"))
            }
        });
        match result {
            Ok(()) => return Ok(()),
            Err(error) if attempt == 0 && !delivered => {
                crate::diagnostics::record("tts_retry", &format!("{error:#}; retry 1/1"), None)
            }
            Err(error) => return Err(error.context("TTS synthesis failed")),
        }
    }
    Err(anyhow::anyhow!("TTS retry budget exhausted"))
}

#[derive(Default)]
pub struct PcmAligner {
    carry: Option<u8>,
}
impl PcmAligner {
    pub fn push(&mut self, input: &[u8]) -> Bytes {
        let mut bytes = Vec::with_capacity(input.len() + 1);
        if let Some(carry) = self.carry.take() {
            bytes.push(carry);
        }
        bytes.extend_from_slice(input);
        if bytes.len() % 2 == 1 {
            self.carry = bytes.pop();
        }
        Bytes::from(bytes)
    }
    pub fn finish(self) -> anyhow::Result<()> {
        if self.carry.is_some() {
            bail!("TTS returned a truncated PCM sample");
        }
        Ok(())
    }
}

pub fn speak_url(config: &Tts, websocket: bool) -> anyhow::Result<reqwest::Url> {
    let mut url = reqwest::Url::parse(if websocket {
        &config.websocket_url
    } else {
        &config.url
    })?;
    url.query_pairs_mut()
        .append_pair("model", &config.model)
        .append_pair("encoding", &config.encoding)
        .append_pair("sample_rate", &config.sample_rate.to_string())
        .append_pair("speed", &config.speed.to_string());
    if !websocket {
        url.query_pairs_mut()
            .append_pair("container", &config.container);
    }
    Ok(url)
}

async fn send_pcm(
    output: &mpsc::Sender<Bytes>,
    data: Bytes,
    cancel: &CancellationToken,
) -> anyhow::Result<()> {
    // Bound IPC packets to 80 ms, preserving even sample boundaries.
    for part in data.chunks(3840) {
        tokio::select! {_=cancel.cancelled()=>return Ok(()),result=output.send(Bytes::copy_from_slice(part))=>result.context("PCM receiver closed")?}
    }
    Ok(())
}

#[async_trait]
impl TtsBackend for HttpTts {
    async fn stream(
        &self,
        text: String,
        output: mpsc::Sender<Bytes>,
        cancel: CancellationToken,
    ) -> anyhow::Result<()> {
        let response = tokio::select! {
            _=cancel.cancelled()=>return Ok(()),
            response=self.client.post(speak_url(&self.config,false)?).header("Authorization",crate::secrets::deepgram_header(&self.key)?).json(&serde_json::json!({"text":text})).send()=>response?,
        };
        crate::diagnostics::record("tts", &format!("HTTP {}", response.status().as_u16()), None);
        if !response.status().is_success() {
            let detail = crate::diagnostics::provider_error(response).await;
            crate::diagnostics::record("tts", &detail, None);
            bail!("TTS provider returned {detail}");
        }
        let mut stream = response.bytes_stream();
        let mut align = PcmAligner::default();
        loop {
            let bytes =
                tokio::select! {_=cancel.cancelled()=>return Ok(()),bytes=stream.next()=>bytes};
            let Some(bytes) = bytes else {
                break;
            };
            let bytes = align.push(&bytes?);
            crate::diagnostics::record("tts_bytes", &bytes.len().to_string(), None);
            if !bytes.is_empty() {
                send_pcm(&output, bytes, &cancel).await?;
            }
        }
        align.finish()
    }
}

#[async_trait]
impl TtsBackend for WebSocketTts {
    async fn stream(
        &self,
        text: String,
        output: mpsc::Sender<Bytes>,
        cancel: CancellationToken,
    ) -> anyhow::Result<()> {
        let mut request = speak_url(&self.config, true)?
            .as_str()
            .into_client_request()?;
        request
            .headers_mut()
            .insert("Authorization", crate::secrets::deepgram_header(&self.key)?);
        let (mut socket, _) = tokio::select! {_=cancel.cancelled()=>return Ok(()),result=tokio::time::timeout(std::time::Duration::from_secs(8),connect_async_with_config(request,None,true))=>result??};
        crate::diagnostics::record("tts", "WebSocket HTTP 101 open", None);
        for message in [
            serde_json::json!({"type":"Speak","text":text}).to_string(),
            r#"{"type":"Flush"}"#.to_owned(),
        ] {
            tokio::select! {
                _ = cancel.cancelled() => return Ok(()),
                result = tokio::time::timeout(std::time::Duration::from_secs(3), socket.send(Message::Text(message.into()))) => result??,
            }
        }
        let mut align = PcmAligner::default();
        loop {
            let message = tokio::select! {_=cancel.cancelled()=>break,message=tokio::time::timeout(std::time::Duration::from_secs(30),socket.next())=>message?};
            match message {
                Some(Ok(Message::Binary(data))) => {
                    crate::diagnostics::record("tts_bytes", &data.len().to_string(), None);
                    let data = align.push(&data);
                    if !data.is_empty() {
                        send_pcm(&output, data, &cancel).await?;
                    }
                }
                Some(Ok(Message::Text(text))) => {
                    let value: serde_json::Value = serde_json::from_str(&text)?;
                    if value["type"] == "Flushed" {
                        break;
                    }
                    if value["type"] == "Error" {
                        bail!(
                            "TTS websocket provider error: {}",
                            crate::diagnostics::redact(&value.to_string())
                        );
                    }
                }
                Some(Ok(Message::Ping(data))) => {
                    tokio::select! {
                        _ = cancel.cancelled() => return Ok(()),
                        result = tokio::time::timeout(std::time::Duration::from_secs(3), socket.send(Message::Pong(data))) => result??,
                    }
                }
                Some(Err(error)) => return Err(error.into()),
                None | Some(Ok(Message::Close(_))) => {
                    bail!("TTS websocket closed before Flush completed")
                }
                _ => {}
            }
        }
        let _ = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            socket.send(Message::Text(r#"{"type":"Close"}"#.into())),
        )
        .await;
        align.finish()
    }
}

#[async_trait]
impl TtsBackend for MockTts {
    async fn stream(
        &self,
        text: String,
        output: mpsc::Sender<Bytes>,
        cancel: CancellationToken,
    ) -> anyhow::Result<()> {
        // Offline test audio is an explicitly synthetic tone, never a fake provider reply.
        let samples = (text.chars().count() * 1100).clamp(4800, 144000);
        for start in (0..samples).step_by(960) {
            let data: Vec<u8> = (start..(start + 960).min(samples))
                .flat_map(|i| {
                    let envelope =
                        (i as f32 / 240.0).min(1.0) * ((samples - i) as f32 / 240.0).min(1.0);
                    ((i as f32 * std::f32::consts::TAU * 220.0 / 24000.0)
                        .sin()
                        .mul_add(2000.0 * envelope, 0.0) as i16)
                        .to_le_bytes()
                })
                .collect();
            tokio::select! {_=cancel.cancelled()=>return Ok(()),_=tokio::time::sleep(std::time::Duration::from_millis(5))=>{}}
            send_pcm(&output, Bytes::from(data), &cancel).await?;
        }
        Ok(())
    }
}

struct Slot {
    id: u32,
    audio: mpsc::Receiver<Bytes>,
}
pub fn backend(state: &Shared) -> Arc<dyn TtsBackend> {
    let config = state.config.current.read().tts.clone();
    let key = zeroize::Zeroizing::new(state.keys.read().deepgram.clone());
    if state.mock {
        Arc::new(MockTts)
    } else if config.transport == "websocket" {
        Arc::new(WebSocketTts { config, key })
    } else {
        Arc::new(HttpTts {
            client: state.client.clone(),
            config,
            key,
        })
    }
}

/// Header: little-endian u32 turn, u32 sentence, u32 flags. Flags 1 =
/// sentence finished, 2 = turn finished. Remaining bytes are raw mono PCM16.
pub fn packet(turn: u32, sentence: u32, flags: u32, pcm: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(12 + pcm.len());
    bytes.extend_from_slice(&turn.to_le_bytes());
    bytes.extend_from_slice(&sentence.to_le_bytes());
    bytes.extend_from_slice(&flags.to_le_bytes());
    bytes.extend_from_slice(pcm);
    bytes
}

pub async fn pipeline(
    state: Shared,
    turn: u32,
    mut text: mpsc::Receiver<String>,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    let backend = backend(&state);
    let channel = state
        .audio_out
        .read()
        .clone()
        .context("El reproductor aún no está listo")?;
    let mut slots = VecDeque::<Slot>::new();
    let mut jobs = FuturesUnordered::<BoxFuture<'static, anyhow::Result<()>>>::new();
    let mut closed = false;
    let mut next_id = 0;
    let mut pcm_bytes = 0u64;
    while !closed || !slots.is_empty() || !jobs.is_empty() {
        tokio::select! {
            biased;
            _=cancel.cancelled()=>return Ok(()),
            result=jobs.next(),if !jobs.is_empty()=>{if let Some(result)=result{result?;}},
            next=text.recv(),if !closed && slots.len()<2=>match next {
                Some(text)=>{
                    next_id+=1;let id=next_id;
                    state.event_tx.send(InputEvent::SentenceQueued{turn,sentence:id,text:text.clone()}).await?;
                    let(tx,rx)=mpsc::channel(16);slots.push_back(Slot{id,audio:rx});
                    let backend=backend.clone();let token=cancel.child_token();
                    let timings=state.clone();
                    jobs.push(Box::pin(async move{timings.latency.mark(turn,"tts_request_sent");timed_stream(backend.as_ref(),text,tx,token).await}));
                }
                None=>closed=true,
            },
            data=async {match slots.front_mut(){Some(slot)=>slot.audio.recv().await,None=>std::future::pending().await}},if !slots.is_empty()=>{
                if let Some(slot)=slots.front() {
                    match data {
                        Some(data)=>{pcm_bytes += data.len() as u64;crate::diagnostics::record("pcm_sent", &format!("sentence={}, bytes={}", slot.id, data.len()), Some(turn));state.latency.mark(turn,"first_tts_byte");channel.send(tauri::ipc::InvokeResponseBody::Raw(packet(turn,slot.id,0,&data)))?;},
                        None=>{channel.send(tauri::ipc::InvokeResponseBody::Raw(packet(turn,slot.id,1,&[])))?;slots.pop_front();},
                    }
                }
            }
        }
    }
    channel.send(tauri::ipc::InvokeResponseBody::Raw(packet(turn, 0, 2, &[])))?;
    state
        .event_tx
        .send(InputEvent::PlaybackQueued {
            turn,
            duration_ms: pcm_bytes / 48,
        })
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{self, Response};

    #[tokio::test(start_paused = true)]
    async fn first_byte_timeout_retries_once_and_partial_audio_never_retries() {
        struct Fixture {
            calls: std::sync::atomic::AtomicUsize,
            partial: bool,
        }
        #[async_trait]
        impl TtsBackend for Fixture {
            async fn stream(
                &self,
                _: String,
                tx: mpsc::Sender<Bytes>,
                _: CancellationToken,
            ) -> anyhow::Result<()> {
                self.calls
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if self.partial {
                    tx.send(Bytes::from_static(&[0, 1])).await?;
                    bail!("TTS fixture disconnected after PCM");
                }
                std::future::pending().await
            }
        }
        for partial in [false, true] {
            let fixture = Fixture {
                calls: std::sync::atomic::AtomicUsize::new(0),
                partial,
            };
            let (tx, mut rx) = mpsc::channel(16);
            let start = tokio::time::Instant::now();
            let error = timed_stream(&fixture, "Hola".into(), tx, CancellationToken::new())
                .await
                .unwrap_err();
            assert_eq!(
                fixture.calls.load(std::sync::atomic::Ordering::Relaxed),
                if partial { 1 } else { 2 }
            );
            if partial {
                assert_eq!(rx.recv().await.unwrap().as_ref(), &[0, 1]);
            } else {
                assert!(format!("{error:#}").contains("timeout"));
                assert_eq!(start.elapsed().as_secs(), 10);
            }
            assert!(rx.recv().await.is_none());
        }
    }

    #[tokio::test]
    async fn sine_wave_survives_odd_network_fragments_and_ipc_packets() {
        let expected: Vec<i16> = (0..24000)
            .map(|i| ((i as f64 * std::f64::consts::TAU * 440.0 / 24000.0).sin() * 20000.0) as i16)
            .collect();
        let raw: Vec<u8> = expected.iter().flat_map(|v| v.to_le_bytes()).collect();
        for width in [1, 3, 7, 3841] {
            let (tx, mut rx) = mpsc::channel(16);
            let send = async {
                let mut align = PcmAligner::default();
                for chunk in raw.chunks(width) {
                    send_pcm(&tx, align.push(chunk), &CancellationToken::new())
                        .await
                        .unwrap();
                }
                align.finish().unwrap();
                drop(tx);
            };
            let receive = async {
                let mut decoded = Vec::new();
                while let Some(pcm) = rx.recv().await {
                    let encoded = packet(42, 1, 0, &pcm);
                    assert_eq!(encoded.len() % 2, 0);
                    assert!(encoded.len() <= 3852);
                    assert_eq!(u32::from_le_bytes(encoded[..4].try_into().unwrap()), 42);
                    decoded.extend(
                        encoded[12..]
                            .as_chunks::<2>()
                            .0
                            .iter()
                            .map(|sample| i16::from_le_bytes([sample[0], sample[1]])),
                    );
                }
                decoded
            };
            let (_, decoded) = tokio::join!(send, receive);
            assert_eq!(decoded, expected);
        }
    }

    #[tokio::test]
    async fn production_http_forwards_pcm_before_eof() {
        let (release, gate) = tokio::sync::oneshot::channel();
        let fixture = Response {
            status: 200,
            chunks: vec![vec![1, 2, 3], vec![4, 5], vec![6]],
            after_first: Some(gate),
        };
        let (url, server) = test_support::http(vec![fixture]).await;
        let mut config = crate::config::Config::parse("").unwrap().tts;
        config.url = url;
        let backend = HttpTts {
            client: test_support::client(),
            config,
            key: zeroize::Zeroizing::new("fixture-key".into()),
        };
        let (tx, mut rx) = mpsc::channel(16);
        let task = tokio::spawn(async move {
            backend
                .stream("Hola.".into(), tx, CancellationToken::new())
                .await
        });
        let first = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&first[..], &[1, 2]);
        assert!(
            !task.is_finished(),
            "first PCM must arrive before the response is complete"
        );
        release.send(()).unwrap();
        let mut audio = first.to_vec();
        while let Some(bytes) = rx.recv().await {
            assert_eq!(bytes.len() % 2, 0);
            audio.extend(bytes);
        }
        task.await.unwrap().unwrap();
        assert_eq!(audio, vec![1, 2, 3, 4, 5, 6]);
        let captured = server.await.unwrap();
        assert!(captured[0].headers.contains("container=none"));
        assert!(captured[0]
            .headers
            .to_lowercase()
            .contains("authorization: token fixture-key"));
        assert_eq!(captured[0].body["text"], "Hola.");
    }

    #[tokio::test]
    async fn production_http_rejects_partial_pcm_sample() {
        let (url, server) = test_support::http(vec![Response::ok(&[1, 2, 3])]).await;
        let mut config = crate::config::Config::parse("").unwrap().tts;
        config.url = url;
        let backend = HttpTts {
            client: test_support::client(),
            config,
            key: zeroize::Zeroizing::new("fixture-key".into()),
        };
        let (tx, _rx) = mpsc::channel(16);
        assert!(backend
            .stream("Hola.".into(), tx, CancellationToken::new())
            .await
            .is_err());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn production_websocket_speak_flush_pcm_and_close() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut config = crate::config::Config::parse("").unwrap().tts;
        config.websocket_url = format!("ws://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            tokio::time::timeout(std::time::Duration::from_secs(5), async move {
                let (tcp, _) = listener.accept().await.unwrap();
                let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
                let speak: serde_json::Value =
                    serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap())
                        .unwrap();
                assert_eq!(speak, serde_json::json!({"type":"Speak","text":"Hola."}));
                assert_eq!(
                    ws.next().await.unwrap().unwrap().to_text().unwrap(),
                    r#"{"type":"Flush"}"#
                );
                ws.send(Message::Binary(vec![1, 2, 3].into()))
                    .await
                    .unwrap();
                ws.send(Message::Binary(vec![4].into())).await.unwrap();
                ws.send(Message::Text(
                    r#"{"type":"Flushed","sequence_id":0}"#.into(),
                ))
                .await
                .unwrap();
                assert_eq!(
                    ws.next().await.unwrap().unwrap().to_text().unwrap(),
                    r#"{"type":"Close"}"#
                );
            })
            .await
            .unwrap();
        });
        let backend = WebSocketTts {
            config,
            key: zeroize::Zeroizing::new("fixture-key".into()),
        };
        let (tx, mut rx) = mpsc::channel(16);
        backend
            .stream("Hola.".into(), tx, CancellationToken::new())
            .await
            .unwrap();
        let mut audio = Vec::new();
        while let Some(bytes) = rx.recv().await {
            audio.extend(bytes);
        }
        assert_eq!(audio, vec![1, 2, 3, 4]);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn cancelled_pcm_backpressure_returns_promptly() {
        let (tx, _rx) = mpsc::channel(1);
        tx.send(Bytes::from_static(&[0, 0])).await.unwrap();
        let cancel = CancellationToken::new();
        let token = cancel.clone();
        let task =
            tokio::spawn(async move { send_pcm(&tx, Bytes::from_static(&[1, 2]), &token).await });
        tokio::task::yield_now().await;
        cancel.cancel();
        tokio::time::timeout(std::time::Duration::from_millis(100), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
    #[test]
    fn aligns_every_fragment() {
        let expected: Vec<u8> = (0..100).collect();
        for split in 1..100 {
            let mut a = PcmAligner::default();
            let mut got = Vec::new();
            for part in expected.chunks(split) {
                got.extend(a.push(part));
            }
            a.finish().unwrap();
            assert_eq!(got, expected);
        }
    }
    #[test]
    fn rejects_truncated_sample() {
        let mut a = PcmAligner::default();
        assert!(a.push(&[1]).is_empty());
        assert!(a.finish().is_err());
    }
    #[test]
    fn binary_packet_header() {
        assert_eq!(
            &packet(1, 2, 0, &[3, 4]),
            &[1, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 3, 4]
        );
    }
    #[test]
    fn http_has_raw_container() {
        let c = crate::config::Config::parse("").unwrap();
        assert!(speak_url(&c.tts, false)
            .unwrap()
            .as_str()
            .contains("container=none"));
        assert!(!speak_url(&c.tts, true)
            .unwrap()
            .as_str()
            .contains("container"));
    }
}
