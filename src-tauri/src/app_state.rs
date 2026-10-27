use parking_lot::RwLock;
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use tauri::Emitter;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Idle,
    Listening,
    Thinking,
    Speaking,
}
impl Phase {
    fn event(self) -> &'static str {
        match self {
            Self::Idle => "state://idle",
            Self::Listening => "state://listening",
            Self::Thinking => "state://thinking",
            Self::Speaking => "state://speaking",
        }
    }
}

pub enum InputEvent {
    SpeechStarted,
    SustainedVoice,
    RemoteSpeechStarted,
    Transcript {
        text: String,
        speech_end: std::time::Instant,
    },
    Mute(bool),
    AudioReady {
        greeting: bool,
    },
    TestVoice,
    SentenceQueued {
        turn: u32,
        sentence: u32,
        text: String,
    },
    Playback {
        turn: u32,
        sentence: u32,
        kind: String,
    },
    PlaybackQueued {
        turn: u32,
        duration_ms: u64,
    },
    ReplyFailed {
        turn: u32,
        message: String,
    },
    Background {
        text: String,
        summarize: bool,
    },
}

pub struct AppState {
    pub shutdown: CancellationToken,
    pub muted: AtomicBool,
    pub microphone_test_until: RwLock<Option<std::time::Instant>>,
    pub stt_retry_after: RwLock<Option<std::time::Instant>>,
    pub config: Arc<crate::config::ConfigStore>,
    pub keys: RwLock<crate::secrets::Keys>,
    pub client: reqwest::Client,
    pub websocket_tls: Arc<rustls::ClientConfig>,
    last_prewarm: parking_lot::Mutex<Option<std::time::Instant>>,
    pub mock: bool,
    pub audio_tx: mpsc::Sender<bytes::Bytes>,
    pub event_tx: mpsc::Sender<InputEvent>,
    pub omit_optional: Arc<AtomicBool>,
    pub audio_out: RwLock<Option<tauri::ipc::Channel>>,
    pub next_turn: std::sync::atomic::AtomicU32,
    pub history_debug: RwLock<Vec<serde_json::Value>>,
    pub latency: crate::latency::LatencyBook,
    pub coding: crate::dispatch::CodingManager,
    pub last_voice: Arc<RwLock<std::time::Instant>>,
    pub started_turn: tokio::sync::watch::Sender<u32>,
    pub settings_save: tokio::sync::Mutex<()>,
    phase: RwLock<Phase>,
}
pub type Shared = Arc<AppState>;

impl AppState {
    pub fn new(
        dir: std::path::PathBuf,
        audio_tx: mpsc::Sender<bytes::Bytes>,
        event_tx: mpsc::Sender<InputEvent>,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            shutdown: CancellationToken::new(),
            muted: AtomicBool::new(false),
            microphone_test_until: RwLock::new(None),
            stt_retry_after: RwLock::new(None),
            config: Arc::new(crate::config::ConfigStore::load(dir)?),
            keys: RwLock::new(crate::secrets::Keys::default()),
            client: reqwest::Client::builder()
                .use_rustls_tls()
                .tcp_nodelay(true)
                .pool_idle_timeout(Duration::from_secs(300))
                .pool_max_idle_per_host(4)
                .connect_timeout(Duration::from_secs(8))
                .read_timeout(Duration::from_secs(30))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            websocket_tls: Arc::new(
                rustls::ClientConfig::builder_with_provider(Arc::new(
                    rustls::crypto::ring::default_provider(),
                ))
                .with_safe_default_protocol_versions()?
                .with_root_certificates(rustls::RootCertStore {
                    roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
                })
                .with_no_client_auth(),
            ),
            last_prewarm: parking_lot::Mutex::new(None),
            mock: std::env::var("JARVIS_MOCK").is_ok_and(|v| v == "1"),
            audio_tx,
            event_tx,
            omit_optional: Arc::new(AtomicBool::new(false)),
            audio_out: RwLock::new(None),
            next_turn: std::sync::atomic::AtomicU32::new(1),
            history_debug: RwLock::new(Vec::new()),
            latency: crate::latency::LatencyBook::default(),
            coding: crate::dispatch::CodingManager::default(),
            last_voice: Arc::new(RwLock::new(std::time::Instant::now())),
            started_turn: tokio::sync::watch::channel(0).0,
            settings_save: tokio::sync::Mutex::new(()),
            phase: RwLock::new(Phase::Idle),
        })
    }

    pub fn toggle_mute(&self, app: &tauri::AppHandle) {
        let muted = !self.muted.fetch_xor(true, Ordering::SeqCst);
        let _ = app.emit("mic://muted", muted);
        let _ = self.event_tx.try_send(InputEvent::Mute(muted));
    }

    pub fn phase(&self) -> Phase {
        *self.phase.read()
    }
    pub fn set_phase(&self, app: &tauri::AppHandle, phase: Phase) {
        let mut current = self.phase.write();
        if *current != phase {
            *current = phase;
            crate::diagnostics::record(
                "phase",
                phase.event(),
                self.next_turn
                    .load(Ordering::Relaxed)
                    .checked_sub(1)
                    .filter(|id| *id > 0),
            );
            let _ = app.emit(phase.event(), ());
        }
    }

    pub async fn prewarm(&self) {
        if self.mock {
            return;
        }
        {
            let mut last = self.last_prewarm.lock();
            if last.is_some_and(|time| time.elapsed() < Duration::from_secs(60)) {
                return;
            }
            *last = Some(std::time::Instant::now());
        }
        let config = self.config.current.read().clone();
        let xai = self
            .client
            .head(format!(
                "{}/models",
                config.llm.base_url.trim_end_matches('/')
            ))
            .timeout(Duration::from_secs(5))
            .send();
        let deepgram = self
            .client
            .head(&config.stt.validation_url)
            .timeout(Duration::from_secs(5))
            .send();
        let tls = async {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let url = reqwest::Url::parse(&config.stt.url)?;
            let host = url
                .host_str()
                .ok_or_else(|| anyhow::anyhow!("Missing STT host"))?;
            let tcp = tokio::net::TcpStream::connect((host, url.port().unwrap_or(443))).await?;
            tcp.set_nodelay(true)?;
            let connector = tokio_rustls::TlsConnector::from(self.websocket_tls.clone());
            let mut socket = connector
                .connect(
                    rustls::pki_types::ServerName::try_from(host.to_owned())?,
                    tcp,
                )
                .await?;
            // Read a response so TLS 1.3 session tickets enter the shared cache.
            // No STT WebSocket or audio stream is opened by this warm-up.
            socket
                .write_all(
                    format!("HEAD / HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n")
                        .as_bytes(),
                )
                .await?;
            let mut response = [0; 4096];
            let _ = socket.read(&mut response).await?;
            Ok::<(), anyhow::Error>(())
        };
        tokio::select! { _ = self.shutdown.cancelled() => {}, _ = async { let _ = tokio::join!(xai, deepgram, tokio::time::timeout(Duration::from_secs(5), tls)); } => {} }
    }
}
