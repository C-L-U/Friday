use parking_lot::RwLock;
use serde::Serialize;
use std::{collections::BTreeMap, path::Path, sync::LazyLock};
use tauri::Emitter;

static FIELDS: LazyLock<RwLock<BTreeMap<String, String>>> = LazyLock::new(RwLock::default);
static APP: RwLock<Option<tauri::AppHandle>> = RwLock::new(None);
static KEYS: RwLock<Vec<zeroize::Zeroizing<String>>> = RwLock::new(Vec::new());
tokio::task_local! { pub static TURN: u32; }

#[derive(Clone, Serialize)]
pub struct Event {
    pub stage: String,
    pub detail: String,
    pub turn: Option<u32>,
}

pub fn init(
    app: &tauri::AppHandle,
    dir: &Path,
) -> anyhow::Result<tracing_appender::non_blocking::WorkerGuard> {
    let writer = tracing_appender::rolling::RollingFileAppender::builder()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("jarvis")
        .filename_suffix("jsonl")
        .max_log_files(7)
        .build(dir)?;
    let (writer, guard) = tracing_appender::non_blocking(writer);
    tracing_subscriber::fmt()
        .json()
        .with_ansi(false)
        .with_env_filter("jarvis=info")
        .with_writer(writer)
        .try_init()
        .map_err(|e| anyhow::anyhow!("log initialization: {e}"))?;
    *APP.write() = Some(app.clone());
    std::panic::set_hook(Box::new(|info| {
        // Do not emit payloads, which can contain credential-bearing request data.
        tracing::error!(location = ?info.location(), "task panic; payload suppressed");
    }));
    record("logs", &dir.display().to_string(), None);
    Ok(guard)
}

pub fn protect(keys: &crate::secrets::Keys) {
    let mut known = KEYS.write();
    for value in [&keys.deepgram, &keys.xai] {
        if !value.is_empty() && !known.iter().any(|k| k.as_str() == value) {
            known.push(zeroize::Zeroizing::new(value.clone()));
        }
    }
}

pub fn redact(text: &str) -> String {
    let mut text = text.to_owned();
    for key in KEYS.read().iter() {
        text = text.replace(key.as_str(), "[redacted]");
    }
    text
}

pub fn record(stage: &str, detail: &str, turn: Option<u32>) {
    let turn = turn.or_else(|| TURN.try_with(|id| *id).ok());
    let detail = redact(detail);
    tracing::info!(stage, detail, turn, "pipeline");
    let event = Event {
        stage: stage.into(),
        detail,
        turn,
    };
    FIELDS
        .write()
        .insert(event.stage.clone(), event.detail.clone());
    if let Some(app) = APP.read().as_ref() {
        let _ = app.emit("diagnostics://update", &event);
    }
}

pub fn error(code: &str, detail: &str, turn: Option<u32>) {
    record("last_error", &format!("{code}: {detail}"), turn);
    if let Some(app) = APP.read().as_ref() {
        let _ = app.emit("state://error", code);
    }
}

pub async fn checked<T>(
    name: &str,
    future: impl std::future::Future<Output = anyhow::Result<T>>,
) -> anyhow::Result<T> {
    use futures_util::FutureExt;
    match std::panic::AssertUnwindSafe(future).catch_unwind().await {
        Ok(result) => result.inspect_err(|error| self::error(name, &format!("{error:#}"), None)),
        Err(_) => {
            // Panic payloads can include request data. Never serialize them.
            self::error(name, "task panicked; operation stopped", None);
            Err(anyhow::anyhow!("{name}: task panicked"))
        }
    }
}

pub fn spawn(name: &'static str, future: impl std::future::Future<Output = ()> + Send + 'static) {
    tauri::async_runtime::spawn(async move {
        let _ = checked(name, async {
            future.await;
            Ok(())
        })
        .await;
    });
}

pub fn snapshot() -> BTreeMap<String, String> {
    FIELDS.read().clone()
}

pub async fn provider_error(mut response: reqwest::Response) -> String {
    let status = response.status().as_u16();
    let read = async {
        let mut bytes = Vec::new();
        while bytes.len() < 2048 {
            match response.chunk().await {
                Ok(Some(chunk)) => {
                    bytes.extend_from_slice(&chunk[..chunk.len().min(2048 - bytes.len())])
                }
                _ => break,
            }
        }
        String::from_utf8_lossy(&bytes).into_owned()
    };
    let body = tokio::time::timeout(std::time::Duration::from_secs(2), read)
        .await
        .unwrap_or_else(|_| "error body timeout".into());
    redact(&format!("HTTP {status}: {body}"))
}
