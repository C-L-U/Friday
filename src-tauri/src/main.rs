#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app_state;
mod audio_in;
mod chunker;
mod commands;
mod config;
mod diagnostics;
mod dispatch;
mod latency;
#[cfg(test)]
mod live_tests;
mod llm;
mod secrets;
mod stt;
#[cfg(test)]
mod test_support;
mod tray;
mod tts;
mod turn;

use tauri::Manager;
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_global_shortcut::ShortcutState;

fn main() {
    if std::env::var("JARVIS_MOCK").is_ok_and(|v| v == "1")
        && std::env::args().any(|arg| arg == "--mock-coding-agent")
    {
        if let Err(error) = dispatch::mock_cli_main() {
            eprintln!("{error}");
            std::process::exit(1);
        }
        return;
    }
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--autostart"]),
        ))
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _, event| {
                    if event.state() == ShortcutState::Pressed {
                        app.state::<app_state::Shared>().toggle_mute(app);
                    }
                })
                .build(),
        )
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let mut logs = app.path().app_data_dir()?.join("logs");
            if std::env::var("JARVIS_MOCK").is_ok_and(|v| v == "1") {
                logs = logs.join("mock");
            }
            app.manage(diagnostics::init(app.handle(), &logs)?);
            let dir = if std::env::var("JARVIS_MOCK").is_ok_and(|v| v == "1") {
                app.path().app_config_dir()?.join("mock")
            } else {
                app.path().app_config_dir()?
            };
            let (audio_tx, audio_rx) = tokio::sync::mpsc::channel(100);
            let (event_tx, event_rx) = tokio::sync::mpsc::channel(100);
            let state = std::sync::Arc::new(app_state::AppState::new(dir, audio_tx, event_tx)?);
            app.manage(state.clone());
            let dev_origin = app.config().build.dev_url.as_ref().map(|url| url.origin());
            let builder =
                tauri::WebviewWindowBuilder::from_config(app, &app.config().app.windows[0])?;
            #[cfg(windows)]
            let builder =
                builder.additional_browser_args("--autoplay-policy=no-user-gesture-required");
            let permission_origin = dev_origin.clone();
            builder
                .on_permission_request(move |webview, kind| {
                    let own_origin = webview.url().is_ok_and(|url| {
                        (url.scheme() == "https" && url.host_str() == Some("tauri.localhost"))
                            || url.scheme() == "tauri"
                            || (cfg!(debug_assertions)
                                && permission_origin
                                    .as_ref()
                                    .is_some_and(|origin| *origin == url.origin()))
                    });
                    if !own_origin {
                        return tauri::webview::PermissionResponse::Deny;
                    }
                    match kind {
                        tauri::webview::PermissionKind::Microphone => {
                            diagnostics::record(
                                "mic_permission",
                                "granted by native permission handler",
                                None,
                            );
                            tauri::webview::PermissionResponse::Allow
                        }
                        _ => tauri::webview::PermissionResponse::Deny,
                    }
                })
                .on_navigation(move |url| {
                    (url.scheme() == "https" && url.host_str() == Some("tauri.localhost"))
                        || url.scheme() == "tauri"
                        || (cfg!(debug_assertions)
                            && dev_origin
                                .as_ref()
                                .is_some_and(|origin| *origin == url.origin()))
                })
                .build()?;
            tray::setup(app.handle())?;
            crate::diagnostics::spawn(
                "main",
                audio_in::run(
                    state.clone(),
                    app.handle().clone(),
                    audio_rx,
                    state.shutdown.child_token(),
                ),
            );
            crate::diagnostics::spawn(
                "main",
                turn::run(
                    state.clone(),
                    app.handle().clone(),
                    event_rx,
                    state.shutdown.child_token(),
                ),
            );
            crate::diagnostics::spawn(
                "main",
                state
                    .config
                    .clone()
                    .watch(app.handle().clone(), state.shutdown.child_token()),
            );
            // Development and offline testing must not register a transient build on login.
            if !cfg!(debug_assertions) && std::env::var_os("JARVIS_MOCK").is_none() {
                if let Err(error) = app.autolaunch().enable() {
                    tracing::warn!(%error, "autostart unavailable");
                }
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::toggle_window,
            commands::bootstrap,
            commands::save_settings,
            commands::save_prompt,
            commands::restore_prompt,
            commands::audio_frame,
            commands::mock_transcript,
            commands::connect_audio,
            commands::audio_ready,
            commands::playback_event,
            commands::debug_latency,
            commands::debug_history,
            commands::debug_coding_status,
            commands::diagnostics_snapshot,
            commands::frontend_diagnostic,
            commands::secrets_status,
            commands::test_voice,
            commands::test_microphone,
            commands::open_microphone_settings,
            commands::save_audio_devices
        ])
        .build(tauri::generate_context!());
    let app = match app {
        Ok(app) => app,
        Err(error) => {
            diagnostics::error("startup", &format!("{error:#}"), None);
            eprintln!("Jarvis startup failed: {error}");
            std::process::exit(1);
        }
    };
    app.run(|app, event| {
        if let tauri::RunEvent::Exit = event {
            app.state::<app_state::Shared>().shutdown.cancel();
        }
    });
}
