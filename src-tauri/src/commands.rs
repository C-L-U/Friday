#[tauri::command]
pub fn toggle_window(app: tauri::AppHandle) {
    crate::tray::toggle(&app);
}

#[tauri::command]
pub fn audio_frame(
    state: tauri::State<'_, Shared>,
    request: tauri::ipc::Request<'_>,
) -> Result<(), String> {
    let tauri::ipc::InvokeBody::Raw(data) = request.body() else {
        return Err(crate::audio_in::AudioError::InvalidFrame.to_string());
    };
    crate::audio_in::enqueue(state.inner(), data).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn mock_transcript(state: tauri::State<'_, Shared>, text: String) -> Result<(), String> {
    if !state.mock {
        return Err("Solo disponible en modo de prueba".into());
    }
    if text.len() > 8000 {
        return Err("Texto demasiado largo".into());
    }
    state
        .event_tx
        .send(crate::app_state::InputEvent::Transcript {
            text,
            speech_end: std::time::Instant::now(),
        })
        .await
        .map_err(|_| "Asistente detenido".into())
}

use crate::{app_state::Shared, config::Config, secrets::Keys};
use serde::Serialize;
use tauri::Emitter;
use tauri_plugin_global_shortcut::GlobalShortcutExt;

#[derive(Serialize)]
pub struct SecretsStatus {
    deepgram: &'static str,
    xai: &'static str,
}

#[tauri::command]
pub async fn secrets_status(state: tauri::State<'_, Shared>) -> Result<SecretsStatus, String> {
    let keys = if state.mock {
        state.keys.read().clone()
    } else {
        let dir = state.config.dir.clone();
        tokio::task::spawn_blocking(move || crate::secrets::load(&dir))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| crate::diagnostics::redact(&format!("{e:#}")))?
    };
    Ok(SecretsStatus {
        deepgram: if keys.deepgram.is_empty() {
            "missing"
        } else {
            "present"
        },
        xai: if keys.xai.is_empty() {
            "missing"
        } else {
            "present"
        },
    })
}

#[tauri::command]
pub async fn test_voice(state: tauri::State<'_, Shared>) -> Result<(), String> {
    if !state.mock && state.keys.read().deepgram.is_empty() {
        return Err("Falta la clave Deepgram".into());
    }
    *state.microphone_test_until.write() = None;
    crate::diagnostics::record(
        "voice_test",
        "Solicitando voz; esperando confirmación de reproducción",
        None,
    );
    state
        .event_tx
        .send(crate::app_state::InputEvent::TestVoice)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn test_microphone(state: tauri::State<'_, Shared>, enabled: bool) -> Result<(), String> {
    if enabled && !state.mock && state.keys.read().deepgram.is_empty() {
        return Err("Falta la clave Deepgram".into());
    }
    *state.microphone_test_until.write() =
        enabled.then(|| std::time::Instant::now() + std::time::Duration::from_secs(20));
    if enabled {
        *state.stt_retry_after.write() = None;
    }
    crate::diagnostics::record(
        "mic_test",
        if enabled {
            "Habla durante 20 segundos; solo transcripción"
        } else {
            "Finalizado"
        },
        None,
    );
    Ok(())
}

#[tauri::command]
pub async fn open_microphone_settings(app: tauri::AppHandle) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    tokio::task::spawn_blocking(move || {
        app.opener()
            .open_url("ms-settings:privacy-microphone", None::<&str>)
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn save_audio_devices(
    state: tauri::State<'_, Shared>,
    input: String,
    output: String,
) -> Result<(), String> {
    if input.len() > 512 || output.len() > 512 {
        return Err("Dispositivo no válido".into());
    }
    let _guard = state.settings_save.lock().await;
    let mut config = state.config.current.read().clone();
    config.ui.input_device = input;
    config.ui.output_device = output;
    state.config.save(config).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub fn diagnostics_snapshot() -> std::collections::BTreeMap<String, String> {
    crate::diagnostics::snapshot()
}

#[tauri::command]
pub fn frontend_diagnostic(stage: String, detail: String, turn: Option<u32>) -> Result<(), String> {
    if ![
        "audio_context",
        "mic_permission",
        "input_device",
        "output_device",
        "playback",
        "frontend_error",
    ]
    .contains(&stage.as_str())
        || detail.len() > 4000
    {
        return Err("Diagnóstico no válido".into());
    }
    if stage == "frontend_error" {
        crate::diagnostics::error("audio", &detail, turn);
    } else {
        crate::diagnostics::record(&stage, &detail, turn);
    }
    Ok(())
}

#[tauri::command]
pub fn debug_latency(state: tauri::State<'_, Shared>) -> Vec<crate::latency::Report> {
    state.latency.recent()
}

#[tauri::command]
pub fn debug_history(state: tauri::State<'_, Shared>) -> Result<Vec<serde_json::Value>, String> {
    if !state.mock {
        return Err("Solo disponible en modo de prueba".into());
    }
    Ok(state.history_debug.read().clone())
}

#[tauri::command]
pub fn debug_coding_status(state: tauri::State<'_, Shared>) -> crate::dispatch::Status {
    state.coding.status()
}

#[tauri::command]
pub fn connect_audio(state: tauri::State<'_, Shared>, on_audio: tauri::ipc::Channel) {
    *state.audio_out.write() = Some(on_audio);
}

#[tauri::command]
pub async fn audio_ready(state: tauri::State<'_, Shared>, greeting: bool) -> Result<(), String> {
    state
        .event_tx
        .send(crate::app_state::InputEvent::AudioReady { greeting })
        .await
        .map_err(|_| "Asistente detenido".into())
}

#[tauri::command]
pub async fn playback_event(
    state: tauri::State<'_, Shared>,
    turn: u32,
    sentence: u32,
    kind: String,
) -> Result<(), String> {
    if !["started", "sentence", "ended"].contains(&kind.as_str()) {
        return Err("Evento de audio no válido".into());
    }
    state
        .event_tx
        .send(crate::app_state::InputEvent::Playback {
            turn,
            sentence,
            kind,
        })
        .await
        .map_err(|_| "Asistente detenido".into())
}

#[derive(Serialize)]
pub struct Settings {
    config: Config,
    prompt: String,
    deepgram_saved: bool,
    xai_saved: bool,
    mock: bool,
}

#[tauri::command]
pub async fn bootstrap(
    state: tauri::State<'_, Shared>,
    app: tauri::AppHandle,
) -> Result<Settings, String> {
    let dir = state.config.dir.clone();
    let mock = state.mock;
    let keys = tokio::task::spawn_blocking(move || {
        if mock {
            Ok(Keys::default())
        } else {
            crate::secrets::load(&dir)
        }
    })
    .await
    .map_err(|_| "No se pudo cargar el llavero")?
    .map_err(|_| "No se pudieron leer las credenciales")?;
    let saved = (keys.deepgram.is_empty(), keys.xai.is_empty());
    crate::diagnostics::protect(&keys);
    crate::diagnostics::record(
        "secrets",
        &format!("loaded: deepgram={}, xai={}", !saved.0, !saved.1),
        None,
    );
    *state.keys.write() = keys;
    let cloned = state.inner().clone();
    crate::diagnostics::spawn("commands", async move {
        cloned.prewarm().await;
        if cloned.keys.read().ready() {
            let config = cloned.config.current.read().clone();
            let keys = cloned.keys.read().clone();
            let report = validate(&cloned, &config, &keys).await;
            let _ = app.emit("settings://validation", &report);
            if !report.model_listed {
                warn_model(&app);
            }
        }
    });
    Ok(Settings {
        config: state.config.current.read().clone(),
        prompt: state.config.prompt.read().clone(),
        deepgram_saved: !saved.0,
        xai_saved: !saved.1,
        mock: state.mock,
    })
}

#[derive(Clone, Serialize)]
pub struct Validation {
    pub deepgram: bool,
    pub xai: bool,
    pub model_listed: bool,
    pub deepgram_message: String,
    pub xai_message: String,
    pub models: Vec<String>,
}

fn warn_model(app: &tauri::AppHandle) {
    tracing::warn!("configured LLM model was not listed; check settings and account access");
    if let Some(tray) = app.tray_by_id("jarvis") {
        let _ = tray.set_tooltip(Some("Jarvis · revisa el modelo en ajustes"));
    }
}

async fn validate(state: &Shared, config: &Config, keys: &Keys) -> Validation {
    if state.mock {
        return Validation {
            deepgram: true,
            xai: true,
            model_listed: true,
            deepgram_message: "Validada (mock)".into(),
            xai_message: "Validada (mock)".into(),
            models: vec![config.llm.model.clone()],
        };
    }
    let dg = state
        .client
        .get(&config.stt.validation_url)
        .header(
            "Authorization",
            crate::secrets::deepgram_header(&keys.deepgram)
                .unwrap_or_else(|_| reqwest::header::HeaderValue::from_static("Token invalid")),
        )
        .timeout(std::time::Duration::from_secs(10))
        .send();
    let xai = state
        .client
        .get(format!(
            "{}/models",
            config.llm.base_url.trim_end_matches('/')
        ))
        .bearer_auth(&keys.xai)
        .timeout(std::time::Duration::from_secs(10))
        .send();
    let (dg, xai) = tokio::join!(dg, xai);
    fn message(result: &Result<reqwest::Response, reqwest::Error>) -> String {
        match result {
            Ok(r) if r.status().is_success() => "Validada".into(),
            Ok(r) => format!(
                "{} {}",
                r.status().as_u16(),
                match r.status().as_u16() {
                    401 => "clave inválida",
                    403 => "sin permiso en la cuenta",
                    429 => "límite de solicitudes",
                    _ => "error del proveedor",
                }
            ),
            Err(e) => crate::diagnostics::redact(&format!("sin conexión: {e}")),
        }
    }
    let deepgram_message = message(&dg);
    let mut xai_message = message(&xai);
    let deepgram = dg.is_ok_and(|r| r.status().is_success());
    let mut valid_xai = false;
    let mut model_listed = false;
    let mut models = Vec::new();
    if let Ok(response) = xai {
        valid_xai = response.status().is_success();
        if valid_xai {
            if let Ok(value) = response.json::<serde_json::Value>().await {
                models = value["data"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|m| m["id"].as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default();
                model_listed = models.contains(&config.llm.model);
                if !model_listed {
                    xai_message = format!("modelo {} no encontrado en tu cuenta", config.llm.model);
                }
            } else {
                xai_message = "respuesta /models no válida".into();
            }
        }
    }
    Validation {
        deepgram,
        xai: valid_xai,
        model_listed,
        deepgram_message,
        xai_message,
        models,
    }
}

#[tauri::command]
pub async fn save_settings(
    state: tauri::State<'_, Shared>,
    app: tauri::AppHandle,
    config: Config,
    deepgram: String,
    xai: String,
) -> Result<Validation, String> {
    let _saving = state.settings_save.lock().await;
    let deepgram = zeroize::Zeroizing::new(deepgram);
    let xai = zeroize::Zeroizing::new(xai);
    config.validate().map_err(|e| e.to_string())?;
    let mut keys = state.keys.read().clone();
    if !deepgram.trim().is_empty() {
        keys.deepgram = deepgram.trim().to_owned();
    }
    if !xai.trim().is_empty() {
        keys.xai = xai.trim().to_owned();
    }
    crate::diagnostics::protect(&keys);
    let report = validate(state.inner(), &config, &keys).await;
    crate::diagnostics::record(
        "validation",
        &format!(
            "deepgram={}, xai={}, model_listed={}",
            report.deepgram, report.xai, report.model_listed
        ),
        None,
    );
    // Persistence and remote validation are separate facts; an outage must not
    // silently discard entered credentials or prevent changing the model/device.
    {
        if !state.mock {
            let dir = state.config.dir.clone();
            let copy = keys.clone();
            tokio::task::spawn_blocking(move || crate::secrets::save(dir, copy))
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| {
                    let detail = crate::diagnostics::redact(&format!("{e:#}"));
                    crate::diagnostics::error("secrets", &detail, None);
                    detail
                })?;
        }
        let old = state.config.current.read().ui.shortcut_mute.clone();
        if old != config.ui.shortcut_mute {
            app.global_shortcut()
                .register(config.ui.shortcut_mute.as_str())
                .map_err(|_| "El atajo ya está en uso")?;
            let _ = app.global_shortcut().unregister(old.as_str());
        }
        state.config.save(config).await.map_err(|e| e.to_string())?;
        *state.keys.write() = keys;
        let _ = app.emit("settings://saved", ());
        let _ = app.emit("settings://validation", &report);
        if !report.model_listed {
            warn_model(&app);
        }
    }
    Ok(report)
}

#[tauri::command]
pub async fn save_prompt(state: tauri::State<'_, Shared>, prompt: String) -> Result<(), String> {
    state
        .config
        .save_prompt(prompt)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn restore_prompt(state: tauri::State<'_, Shared>) -> Result<String, String> {
    let prompt = crate::config::PROMPT.trim_end().to_owned();
    state
        .config
        .save_prompt(prompt.clone())
        .await
        .map_err(|e| e.to_string())?;
    Ok(prompt)
}
