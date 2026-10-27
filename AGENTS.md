# Jarvis engineering guide

## Architecture and module map

Tauri 2 desktop app; Rust owns all networking, secrets, configuration, turn management and subprocesses. Plain HTML/CSS/ES modules use the bundled Tauri global API, without npm or a bundler. Microphone and playback share the webview so browser acoustic echo cancellation can reference playback.

`src-tauri/src`: main (bootstrap), app_state (state/events), config (embedded defaults and reload), secrets (OS keyring and restricted fallback), audio_in (raw IPC/VAD/500 ms pre-roll), stt (Deepgram streaming), llm (xAI SSE/tool assembly), chunker (spoken clauses/sanitizer), tts (streaming PCM), turn (cancellation/history/wake activation), dispatch (coding CLI lifecycle), tray (tray/shortcut), commands (UI bridge), latency (turn timings).

## Rules

- Use verified Tauri 2 APIs and rustls; no OpenSSL, frontend frameworks, npm build, secret logging or keys in TOML.
- No incomplete functions or panic macros on runtime paths. anyhow at boundaries, thiserror for module errors, tracing for diagnostics.
- Async work must not block; filesystem/keyring blocking operations belong in spawn_blocking or startup. Long-lived tasks own CancellationToken values. Exactly one voice turn; cancellation stops network work and browser playback.
- Binary 16 kHz mono signed PCM input in 20 ms frames; 24 kHz streaming PCM playback in the same webview. Keep 500 ms input pre-roll. Ordered synthesis with at most two concurrent requests.
- Wake word defaults to Jarvis; 20 second conversation window; configurable Spanish STT/TTS and user-editable exact default prompt.
- OS keyring first, restricted fallback file only with warning. Never expose saved keys to JavaScript.
- Coding subprocesses run only in canonicalized project directories, support cancellation/timeouts and never block the conversation. This is working-directory confinement, not an OS sandbox.
- Validate current external API/CLI contracts; document unresolved claims in NOTES.md. Do not claim latency, acoustic or cross-platform runtime verification without measurements.

## Development and checks

Install Rust and the documented Tauri platform build prerequisites plus `cargo install tauri-cli --locked --version '^2'`. Run `cargo tauri dev` at repository root. Build native installers with `cargo tauri build`. There is no frontend build step.

Run `cargo check`, `cargo clippy -- -D warnings`, and `cargo test` at each milestone: scaffold; desktop shell; settings/secrets; audio/STT; LLM/chunker; TTS/playback; turns; latency; dispatch; packaging. `JARVIS_MOCK=1` selects deterministic offline provider implementations. Real provider and acoustic verification requires working API keys and a microphone. Keep test/build evidence in NOTES.md.

When continuing an existing workspace, preserve the implementation and recorded evidence, rerun the checks, and record newly verified results separately from earlier runs. Never label mock audio measurements as live provider or acoustic measurements.
