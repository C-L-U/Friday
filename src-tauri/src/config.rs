use anyhow::{ensure, Context};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tauri_plugin_global_shortcut::GlobalShortcutExt;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

pub const DEFAULTS: &str = include_str!("../defaults/config.toml");
pub const PROMPT: &str = include_str!("../defaults/system_prompt.txt");

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub llm: Llm,
    pub stt: Stt,
    pub tts: Tts,
    pub activation: Activation,
    pub coding: Coding,
    pub ui: Ui,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Llm {
    pub base_url: String,
    pub model: String,
    pub max_tokens: u32,
    pub history_turns: usize,
    pub history_token_budget: usize,
    pub reasoning_effort: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stt {
    pub url: String,
    pub validation_url: String,
    pub model: String,
    pub language: String,
    pub endpointing_ms: u32,
    pub utterance_end_ms: u32,
    pub idle_close_secs: u64,
    pub keyterms: Vec<String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tts {
    pub url: String,
    pub websocket_url: String,
    pub model: String,
    pub speed: f32,
    pub encoding: String,
    pub sample_rate: u32,
    pub container: String,
    pub transport: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Activation {
    pub require_wake_word: bool,
    pub wake_word: String,
    pub conversation_window_secs: u64,
    pub barge_in_ms: u32,
    pub vad_threshold: f32,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Coding {
    pub default_agent: String,
    pub projects_dir: String,
    pub timeout_minutes: u64,
    pub extra_args: Vec<String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ui {
    pub shortcut_mute: String,
    #[serde(default)]
    pub input_device: String,
    #[serde(default)]
    pub output_device: String,
}

fn merge(base: &mut toml::Value, overrides: toml::Value) {
    match (base, overrides) {
        (toml::Value::Table(base), toml::Value::Table(overrides)) => {
            for (key, value) in overrides {
                if let Some(old) = base.get_mut(&key) {
                    merge(old, value);
                } else {
                    base.insert(key, value);
                }
            }
        }
        (base, value) => *base = value,
    }
}

impl Config {
    pub fn parse(overrides: &str) -> anyhow::Result<Self> {
        let mut value = toml::from_str(DEFAULTS)?;
        merge(&mut value, toml::from_str(overrides)?);
        let config: Self = value.try_into()?;
        config.validate()?;
        Ok(config)
    }
    pub fn validate(&self) -> anyhow::Result<()> {
        for (url, scheme) in [
            (&self.llm.base_url, "https"),
            (&self.stt.url, "wss"),
            (&self.stt.validation_url, "https"),
            (&self.tts.url, "https"),
            (&self.tts.websocket_url, "wss"),
        ] {
            let parsed = reqwest::Url::parse(url)?;
            ensure!(
                parsed.scheme() == scheme
                    && parsed.host_str().is_some()
                    && parsed.username().is_empty()
                    && parsed.password().is_none()
                    && parsed.query().is_none(),
                "URL de proveedor no válida"
            );
        }
        ensure!(
            !self.llm.model.is_empty() && !self.tts.model.is_empty(),
            "Falta el modelo"
        );
        ensure!(
            (32..=8192).contains(&self.llm.max_tokens)
                && (1..=100).contains(&self.llm.history_turns)
                && (256..=32000).contains(&self.llm.history_token_budget),
            "Límites de conversación no válidos"
        );
        ensure!(
            (0.7..=1.5).contains(&self.tts.speed)
                && self.tts.sample_rate == 24000
                && self.tts.encoding == "linear16"
                && self.tts.container == "none",
            "El audio requiere PCM lineal a 24 kHz y velocidad entre 0.7 y 1.5"
        );
        ensure!(
            ["http", "websocket"].contains(&self.tts.transport.as_str()),
            "Transporte TTS no válido"
        );
        ensure!(
            ["claude", "codex"].contains(&self.coding.default_agent.as_str())
                && (1..=180).contains(&self.coding.timeout_minutes),
            "Agente o tiempo límite no válido"
        );
        ensure!(
            (100..=2000).contains(&self.stt.endpointing_ms)
                && self.stt.utterance_end_ms >= 1000
                && (2..=60).contains(&self.stt.idle_close_secs),
            "Intervalos STT no válidos"
        );
        ensure!(
            !self.activation.wake_word.trim().is_empty()
                && self.activation.wake_word.chars().all(char::is_alphanumeric)
                && self.activation.wake_word.chars().count() <= 40
                && self.activation.conversation_window_secs <= 3600
                && (100..=1000).contains(&self.activation.barge_in_ms)
                && (0.001..=0.2).contains(&self.activation.vad_threshold),
            "Activación no válida"
        );
        self.ui
            .shortcut_mute
            .parse::<tauri_plugin_global_shortcut::Shortcut>()
            .context("Atajo no válido")?;
        Ok(())
    }
}

pub struct ConfigStore {
    pub dir: PathBuf,
    pub current: RwLock<Config>,
    pub prompt: RwLock<String>,
    writes: Mutex<()>,
}

impl ConfigStore {
    pub fn load(dir: PathBuf) -> anyhow::Result<Self> {
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("config.toml");
        if !path.exists() {
            std::fs::write(&path, DEFAULTS)?;
        }
        let config = match Config::parse(&std::fs::read_to_string(path)?) {
            Ok(config) => config,
            Err(_) => {
                tracing::warn!("invalid configuration; using embedded defaults until corrected");
                Config::parse("")?
            }
        };
        let path = dir.join("system_prompt.txt");
        if !path.exists() {
            std::fs::write(&path, PROMPT.trim_end())?;
        }
        let prompt = std::fs::read_to_string(path)?;
        Ok(Self {
            dir,
            current: RwLock::new(config),
            prompt: RwLock::new(prompt),
            writes: Mutex::new(()),
        })
    }
    pub async fn save(&self, config: Config) -> anyhow::Result<()> {
        config.validate()?;
        let _guard = self.writes.lock().await;
        tokio::fs::write(
            self.dir.join("config.toml"),
            toml::to_string_pretty(&config)?,
        )
        .await?;
        *self.current.write() = config;
        Ok(())
    }
    pub async fn save_prompt(&self, prompt: String) -> anyhow::Result<()> {
        ensure!(prompt.len() <= 32000, "El prompt es demasiado largo");
        let _guard = self.writes.lock().await;
        tokio::fs::write(self.dir.join("system_prompt.txt"), &prompt).await?;
        *self.prompt.write() = prompt;
        Ok(())
    }
    pub async fn watch(self: Arc<Self>, app: tauri::AppHandle, cancel: CancellationToken) {
        let mut interval = tokio::time::interval(Duration::from_millis(750));
        let mut previous = String::new();
        loop {
            tokio::select! { _ = cancel.cancelled() => break, _ = interval.tick() => {} }
            let _guard = self.writes.lock().await;
            if let Ok(text) = tokio::fs::read_to_string(self.dir.join("config.toml")).await {
                if text != previous {
                    previous = text.clone();
                    match Config::parse(&text) {
                        Ok(config) => {
                            let old = self.current.read().ui.shortcut_mute.clone();
                            if old != config.ui.shortcut_mute {
                                if app
                                    .global_shortcut()
                                    .register(config.ui.shortcut_mute.as_str())
                                    .is_err()
                                {
                                    tracing::warn!(
                                        "new shortcut unavailable; keeping previous configuration"
                                    );
                                    continue;
                                }
                                let _ = app.global_shortcut().unregister(old.as_str());
                            }
                            *self.current.write() = config;
                            tracing::info!("configuration reloaded");
                        }
                        Err(_) => {
                            tracing::warn!("invalid configuration; retaining last valid values")
                        }
                    }
                }
            }
            if let Ok(prompt) = tokio::fs::read_to_string(self.dir.join("system_prompt.txt")).await
            {
                if prompt.len() <= 32000 {
                    *self.prompt.write() = prompt;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn partial_overrides() {
        let c = Config::parse("[llm]\nmodel='custom'").unwrap();
        assert_eq!(c.llm.model, "custom");
        assert_eq!(c.stt.language, "es");
        assert!(!c.activation.require_wake_word);
        assert!(c.ui.input_device.is_empty() && c.ui.output_device.is_empty());
    }
    #[test]
    fn rejects_secret_in_config() {
        assert!(Config::parse("api_key='secret'").is_err());
    }
    #[test]
    fn rejects_insecure_endpoint() {
        assert!(Config::parse("[llm]\nbase_url='http://example.com'").is_err());
    }
    #[test]
    fn rejects_overflowing_conversation_window() {
        assert!(
            Config::parse("[activation]\nconversation_window_secs=9223372036854775807").is_err()
        );
        assert!(Config::parse("[activation]\nconversation_window_secs=0").is_ok());
        assert!(Config::parse("[activation]\nconversation_window_secs=3600").is_ok());
    }
}
