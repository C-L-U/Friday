//! Opt-in paid provider checks. Credentials never leave Rust or appear in output.
use crate::{llm::LlmBackend, tts::TtsBackend};
use std::sync::{atomic::AtomicBool, Arc};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[tokio::test]
#[ignore = "requires real Deepgram/xAI keys; makes paid requests"]
async fn live_tts_and_llm() -> anyhow::Result<()> {
    let mut keys = crate::secrets::Keys {
        deepgram: std::env::var("JARVIS_TEST_DEEPGRAM_KEY").unwrap_or_default(),
        xai: std::env::var("JARVIS_TEST_XAI_KEY").unwrap_or_default(),
    };
    if !keys.ready() && std::env::var("JARVIS_TEST_USE_KEYRING").as_deref() == Ok("1") {
        let dir = std::path::PathBuf::from(std::env::var("APPDATA")?).join("com.jarvis.voice");
        keys = tokio::task::spawn_blocking(move || crate::secrets::load(&dir)).await??;
    }
    anyhow::ensure!(
        keys.ready(),
        "Set JARVIS_TEST_DEEPGRAM_KEY and JARVIS_TEST_XAI_KEY"
    );
    crate::diagnostics::protect(&keys);
    let config = crate::config::Config::parse("")?;
    let client = reqwest::Client::builder()
        .use_rustls_tls()
        .connect_timeout(std::time::Duration::from_secs(5))
        .read_timeout(std::time::Duration::from_secs(15))
        .build()?;
    let backend = crate::tts::HttpTts {
        client: client.clone(),
        config: config.tts,
        key: zeroize::Zeroizing::new(keys.deepgram.clone()),
    };
    let (tx, mut rx) = mpsc::channel(16);
    let collect = async {
        let mut bytes = 0;
        let mut nonzero = false;
        while let Some(chunk) = rx.recv().await {
            let chunk: bytes::Bytes = chunk;
            anyhow::ensure!(chunk.len().is_multiple_of(2), "unaligned PCM");
            bytes += chunk.len();
            nonzero |= chunk.iter().any(|b| *b != 0);
        }
        anyhow::ensure!(bytes > 0 && nonzero, "empty/silent provider audio");
        Ok::<_, anyhow::Error>(bytes)
    };
    let (_, bytes) = tokio::try_join!(
        backend.stream(
            "Hola, soy Jarvis, ¿me escuchas?".into(),
            tx,
            CancellationToken::new()
        ),
        collect
    )?;
    let grok = crate::llm::Grok {
        client,
        omit_optional: Arc::new(AtomicBool::new(false)),
    };
    let (tx, mut rx) = mpsc::channel::<String>(32);
    let request = crate::llm::LlmRequest {
        config: config.llm,
        key: zeroize::Zeroizing::new(keys.xai.clone()),
        messages: vec![serde_json::json!({"role":"user", "content":"Responde solamente: Hola."})],
        allow_tools: false,
    };
    let collect = async {
        let mut text = String::new();
        while let Some(token) = rx.recv().await {
            text.push_str(&token);
        }
        text
    };
    let (response, text) =
        tokio::join!(grok.stream(request, tx, CancellationToken::new()), collect);
    anyhow::ensure!(
        !response?.text.trim().is_empty() && !text.trim().is_empty(),
        "empty provider text"
    );
    println!(
        "Live providers passed: {bytes} PCM bytes, {} streamed text bytes",
        text.len()
    );
    Ok(())
}
