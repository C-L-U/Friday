# Jarvis implementation notes

## Production bug-fix pass — diagnosis before fixes (2026-10-26)

- Preserved the existing implementation, including unrecorded initial `diagnostics.rs`, rolling JSONL logs and diagnostic scripts already in the workspace. Added task-local turn correlation and a panic/error boundary. Baseline check/Clippy/86 tests passed with the documented `C:/mingw64/bin` and `CARGO_TARGET_DIR=C:/tmp/jarvis-target`. An initial invocation without the latter reproduced the known windres path-with-spaces failure, not an application failure.
- Built and installed the instrumented NSIS into `C:/tmp/jarvis-bugfix-proof`. `node scripts/diagnose-before.mjs` ran against that installed application with mock providers, no simulated user gesture: `hola` yielded `rejected: wake word missing, conversation window closed`; AudioContext was `running`. `Jarvis, hola` produced binary PCM and `first audio played`. This confirms silent wake rejection and rules out a universally broken Channel/scheduler/autoplay on this machine. It does NOT prove physical headphone output.
- In the same installed reproduction, a successful Save returned `{empty:true, placeholder:'', status:'', validated:true}`. Confirmed UI cause: fields cleared before validation completes, no saved placeholder until restart, empty success message and automatic panel closure. Credential loss is not established.
- The pre-existing real-provider log at `%APPDATA%/com.jarvis.voice/logs/jarvis.2026-10-26.jsonl` records at 17:53:19Z saved presence for both keys, `audio_context=running`, successful getUserMedia and Realtek USB2.0 input. At 17:53:34Z it records TTS HTTP 200 and webview playback acknowledgement; at 17:53:43–44Z xAI HTTP 200, first token, TTS HTTP 200 and playback acknowledgement. These are historical observations, not a new live test. They contradict blanket claims of an invalid model/key or denied microphone.
- Missing browser permission prompt is explained by the existing Tauri `on_permission_request` handler returning Allow for Microphone. Tauri 2.12.5 source confirms the Windows WebView2 implementation; absence of a prompt is not evidence of denial. OS privacy can still deny capture.
- Original user-reported physical silence remains unconfirmed: default output was neither identified nor selectable. No assertion that sound reached the headset will be made from playback timer acknowledgements.
- `JARVIS_TEST_DEEPGRAM_KEY` and `JARVIS_TEST_XAI_KEY` are absent from this process environment. Existing keyring credentials may be exercised inside the app without returning them to JavaScript.

## Continuation audit (2026-10-26)

- This continuation started with all implementation modules, test scripts and Windows installers already present, despite the original empty-directory brief. Earlier evidence below is historical until rerun; preserve this workspace rather than scaffold over it.
- Baseline rerun: `cargo check`, `cargo clippy -- -D warnings`, and all 75 original tests passed.
- Fixed a UTF-8 boundary panic in the chunker after non-ASCII whitespace and made uppercase URLs stay silent. Added regression coverage for both.
- Reject model streams without a terminal completion reason before dispatching tools, including valid argument JSON followed by premature EOF, `[DONE]` alone or a token-limit finish. Ordinary token-limited spoken replies remain allowed. HTTP regression tests cover all terminal combinations.
- Made WebSocket TTS writes cancellation-aware with bounded timeouts, capped the conversation window at one hour to prevent `Instant` overflow, and made error/settings expansion show the native window when hidden in the tray.
- Added local HTTP/WebSocket fixtures that exercise the production Grok, HTTP TTS, WebSocket TTS and STT clients. Verified incremental delivery before EOF, fragmented UTF-8/PCM, optional-parameter retry remembered across turns, Speak/Flush/Close, 25 queued input frames, final transcript assembly and graceful STT cancellation. These validate client protocol handling without claiming provider interoperability or TLS/acoustic measurements.
- Final Rust checks: `cargo check --locked`, `cargo clippy --locked --all-targets -- -D warnings`, `cargo test --locked` (86 passed) and formatting check passed. AudioWorklet and playback Node tests passed. No incomplete-code markers or OpenSSL dependency found.
- `cargo tauri dev` launched successfully. The full WebView smoke test passed in development and in the freshly installed production NSIS app, including error recovery from a hidden window. Windows keeps `document.hidden` false with background throttling disabled; the test observes successful native `show()` instead. The original browser-visibility assertion failed and was corrected.
- Real first-run microphone test passed again: exact default prompt, empty credential inputs, automatic settings panel; 50 binary 640-byte frames from 48 kHz capture, with echo cancellation, noise suppression and automatic gain control reported enabled. No microphone audio was sent to a provider.
- Native keyring probe passed a synthetic write/read/delete cycle. One attempt to compile the probe while the development executable was running hit a Windows resource lock; rerunning after closing that test app passed.
- `cargo tauri build` produced NSIS (about 5.51 MiB) and MSI (7.26 MiB). Updated copies and SHA-256 hashes are in `artifacts/installers/`. The NSIS package was installed to an isolated test directory and passed with PATH restricted to Windows system directories; the temporary installation was then uninstalled successfully. MSI generation passed; MSI installation remains untested.
- Offline first scheduled audio: 169 ms in the initial development test and 168 ms installed. Binary IPC voice onset to stop event: 200 ms development / 197 ms installed, including ten 20 ms frames with the first submitted at time zero. These are single fixture measurements, not a live p50 or physical sound measurement.
- Rechecked installed Claude/Codex help and official Tauri window/config/autostart, Deepgram STT/HTTP+WS TTS and xAI model/chat documentation. Cargo metadata shows the locked Tauri stack requires Rust 1.90; the manifest now states that minimum. This Windows run used Rust 1.98.1 and Tauri CLI 2.12.4.
- Live provider, acoustic and other-platform claims still require the evidence listed under UNVERIFIED.

## Assumptions

- Starting from an empty directory on Windows. Installed developer toolchain and CLI availability are checked locally. End users only need the installer and API keys; source builds require Tauri platform prerequisites.
- User-requested model defaults are retained as configuration and checked against the provider model listing. Model availability is not assumed.
- Browser AEC shares the playback webview; physical echo rejection and latency require real hardware measurements.
- The requested 200 ms sustained-speech filter imposes a minimum around 200 ms from speech onset to interruption, so a 150 ms onset-to-stop guarantee cannot coexist with that filter. Stop dispatch after detection is measured separately.
- `--dangerously-skip-permissions` is never a default. It may only be explicitly configured for coding subprocesses in the projects directory.

## UNVERIFIED

- Live Deepgram/xAI credentials, model entitlement, provider interoperability, HTTP/WebSocket TTS audio quality and home-network p50 latency require real API keys. No paid requests were made.
- Acoustic echo rejection is not established by enabling browser AEC. It needs a loudspeaker/microphone conversation test. The real microphone capture path was exercised on Windows, including all three processing constraints.
- macOS/Linux compilation, installation, microphone permission, system keyring, autostart and global shortcut behavior require their native runners. The workflow is ready but was not submitted to GitHub from this workspace.
- Code signing/notarization was not performed: no publisher certificates were provided. Windows packages are unsigned; distribution reputation/Gatekeeper behavior is not verified.
- Real Claude/Codex authenticated coding sessions were not launched. Installed CLI help verified the flags; JSONL parsing, subprocess execution, progress, summary and voice cancellation were exercised using a real fixture subprocess.
- Cold-start <2 seconds, physical audio onset, idle <2% CPU and total WebView memory are not certified. A debug host snapshot was about 49 MB excluding WebView processes; that is not an end-user resource benchmark.

## Evidence

- Initial workspace inspection: empty; cargo, rustc, git, claude and codex are installed.
- Initial scaffold check exposed the existing MinGW install's unquoted internal path with spaces. Retried with the existing `C:/mingw64/bin` first on PATH. MSVC toolchain is present but Visual Studio linker/SDK are not installed; Windows CI uses MSVC.
- Scaffold `cargo check` passed before features. Milestone check/Clippy/test gates passed in order: desktop shell (0 tests), settings (3), audio/STT (11), LLM/chunker (53), TTS (57), turns (65), latency (66), dispatch (72), packaging and final hardening (75 on Windows; one additional Unix symlink test runs on Unix). Final `cargo clippy -- -D warnings`, `cargo test`, `cargo fmt --all -- --check` and `cargo check` are clean.
- `cargo tauri build` produced both Windows installers: `Jarvis_0.1.0_x64-setup.exe` (about 5.5 MiB) and `Jarvis_0.1.0_x64_es-ES.msi` (about 7.3 MiB). NSIS was silently installed to an isolated test directory, launched with PATH containing only Windows system directories, and passed the WebView smoke test. MSI was generated, not installed. The isolated NSIS test installation was then uninstalled.
- `cargo tauri dev` was executed, exposed an initially over-restrictive development navigation policy, and passed after restricting development navigation to the actual configured dev-server origin. No frontend bundler or npm installation was used.
- `node scripts/smoke-first-run.mjs --microphone` passed against the real first-run development application: settings automatically visible, original prompt exact, no saved key value exposed, attention state. Actual getUserMedia + AudioWorklet emitted 50 binary frames of 640 bytes from a 48 kHz input; echoCancellation, noiseSuppression and autoGainControl all reported true. This test did not send microphone audio to any provider.
- `cargo run --example keyring_probe` passed a write/read/delete cycle in the native Windows credential store using a unique synthetic test entry. It did not access production credentials. Unix 0600 and unavailable-keychain fallback paths have code review evidence, not native runtime evidence here.
- `node scripts/test-audio.mjs` passed four input sample rates (16/44.1/48/96 kHz), exact 20 ms output framing, speech passband and alias rejection. `node scripts/test-playback.mjs` passed gapless sample scheduling, cancellation of sources/timers, rejection of late cancelled PCM and ordered playback acknowledgements.
- `node scripts/smoke-webview.mjs` passed in the debug app and the installed production app, using default autoplay behavior: Thinking → Speaking → Listening through real Rust IPC/Tauri binary Channel/Web Audio. It also passed prompt debounce/restore, raw PCM VAD interruption, conservative spoken history, thinking-turn merge, coding fixture process completion, final spoken summary and cancellation by a streamed tool call.
- Offline examples: first scheduled audio 178 ms in debug and 174 ms installed; binary-IPC speech onset to `stop_audio` event 197/199 ms, including the 200 ms/ten-frame gate (the first frame is submitted at time zero). These are fixture measurements, **not** the requested provider p50 or acoustic stop time. Normal turn logs contained every required timing mark.
- Source scan found no incomplete-code markers. Runtime panic sites are limited to constant regex initialization and desktop startup invariants; test assertions are separate. `cargo tree -i openssl-sys` found no such package.

## Implementation boundaries

- `speech_end` is the last locally voiced frame timestamp sampled when Deepgram endpoints a turn, so endpointing/transit time is included in `final_transcript`. Playback acknowledges the scheduled Web Audio timeline plus the browser's output latency. Exact acoustic latency requires loopback measurement.
- An interrupted sentence is omitted from history until its playback completion acknowledgement. This deliberately retains only complete clauses actually played; it does not guess how many words of an interrupted clause were audible.
- HTTP prewarming uses the shared pooled client. STT additionally warms DNS/TCP/TLS with a bounded unauthenticated HTTP HEAD over the same rustls session cache used by its WebSocket connector; it does not open an idle STT session. Warm-up is repeated on voice activity at most once per minute. TLS resumption still depends on provider support.
- Reconnect audio is bounded to thirty seconds plus a 500 ms replay ring. Overflow is reported and restarts with pre-roll; unlimited outage buffering would violate bounded memory and privacy. Local VAD is an adaptive energy detector, with sustained-voice/DC/noise/impulse tests; it is not Silero and can respond to sustained non-speech noise.
- Settings and system prompt hot reload; an already-open STT session takes new model/language settings on its next connection. New LLM turns and TTS pipelines read the current settings. A rejected optional LLM parameter is disabled for the session after one 4xx retry.
- Coding progress cannot interrupt an active conversation and waits at least 1.5 seconds after local voice. CLI task input goes through stdin, equivalent to the documented noninteractive pipe form, avoiding Windows shell quoting and command-line exposure. Session IDs are held in memory per canonical project path. Working-directory checks do not restrict all filesystem capabilities of an external coding tool.
- Mock mode uses a separate settings subdirectory and never accesses production keyring entries. Its CLI fixture deliberately creates `demo.txt` under the configured projects folder. Only the explicitly run keyring probe accesses a separate temporary test service.

## Verified contracts and references

- Tauri 2.12.5 resolved by Cargo. Raw `InvokeBody::Raw` input and `Channel<InvokeResponseBody>` output are documented at https://v2.tauri.app/develop/calling-rust/ and https://docs.rs/tauri/latest/tauri/ipc/struct.Channel.html . `Vec<u8>` alone would serialize to JSON and is deliberately avoided.
- Window keys, CSP, `withGlobalTauri`, `macOSPrivateApi`, platform overrides: https://v2.tauri.app/reference/config/ . Current docs say macOSPrivateApi is a no-op since 2.12.1; retain it and the matching feature for requested compatibility.
- Tray/menu: https://v2.tauri.app/learn/system-tray/ . Plugins: https://v2.tauri.app/plugin/autostart/ , https://v2.tauri.app/plugin/single-instance/ , https://v2.tauri.app/plugin/global-shortcut/ , https://v2.tauri.app/plugin/opener/ . Core permissions: https://v2.tauri.app/reference/acl/core-permissions/ . Plugins invoked exclusively in Rust need no frontend grants.
- Deepgram STT parameters, KeepAlive, CloseStream, SpeechStarted and `utterance_end_ms` with interim results: https://developers.deepgram.com/reference/speech-to-text/listen-streaming .
- Deepgram HTTP TTS including speed 0.7–1.5: https://developers.deepgram.com/reference/text-to-speech/speak-request . WebSocket Speak/Flush/Flushed/Close: https://developers.deepgram.com/reference/text-to-speech/speak-streaming . Speed is not supported in all languages; provider errors must be surfaced.
- Grok 4.7 is listed at https://docs.x.ai/developers/models/grok-4.7 . Lowest supported reasoning effort is `low`; reasoning cannot be disabled: https://docs.x.ai/developers/model-capabilities/text/reasoning . Chat completions remains supported: https://docs.x.ai/developers/model-capabilities/legacy/chat-completions .
- Both `claude --help` and `codex exec --help` executed locally. Claude confirms print, stream-json, verbose, acceptEdits and resume. Codex confirms exec, json, workspace-write, skip-git-repo-check and resume. https://learn.chatgpt.com/docs/non-interactive-mode documents JSONL events. CLI authentication is separate from the two voice API keys.
- Latest reqwest is 0.13.5 but its renamed TLS features no longer include the requested `rustls-tls`; use the maintained 0.12 release line to preserve the required feature contract. Keyring 4.2 changed its facade; use compatible 3.6.3 with native stores and Linux Secret Service + Rust crypto. See https://docs.rs/keyring/3.6.3/keyring/ .
- Windows WebView2 embedBootstrapper: https://v2.tauri.app/distribute/windows-installer/ .
- Aura-2 Celeste Spanish model confirmed in https://developers.deepgram.com/docs/tts-models .
- Current `WebviewWindowBuilder::on_permission_request` accepts the Microphone permission; production navigation is local only. https://docs.rs/tauri/latest/tauri/struct.WebviewWindowBuilder.html .
- Packaging platform overrides, media framework bundling, infoPlist and entitlements were checked in https://v2.tauri.app/reference/config/ and https://v2.tauri.app/distribute/macos-application-bundle/ . Apple's audio-input entitlement: https://developer.apple.com/documentation/bundleresources/entitlements/com.apple.security.device.audio-input .
- GitHub workflow and current action inputs: https://v2.tauri.app/distribute/pipelines/github/ and https://github.com/tauri-apps/tauri-action . Node in CI runs tests/the action only; application/frontend compilation remains Cargo-only.
- Subprocess tree ownership uses process-wrap 10 JobObject on Windows and ProcessGroup on Unix, with KillOnDrop: https://docs.rs/process-wrap/latest/process_wrap/ . Windows creation flags were checked against the installed crate's typed API and compiled.
- Shared WebSocket rustls connector: https://docs.rs/tokio-tungstenite/latest/tokio_tungstenite/fn.connect_async_tls_with_config.html ; tokio-rustls 0.26.6 TlsConnector API verified in installed crate source.

## Definition-of-done evidence

| Requirement | Evidence / remaining verification |
| --- | --- |
| Fresh source launch | `cargo tauri dev` launches; local toolchain workaround documented in README. |
| First run and secrets | Real first-run UI + native keyring roundtrip passed; actual key validation needs supplied accounts. |
| Prompt editing | Debounced save and exact restore passed; turn construction reads the current prompt for every new turn. |
| Streaming duplex | Production STT/SSE/HTTP+WS TTS implemented; actual mic and offline end-to-end paths passed separately. Live provider loop remains unverified. |
| Barge-in and history | Binary VAD/stop event and history tests passed; ~200 ms gate intentionally limits onset-to-stop. |
| AEC | Capture and playback share Web Audio/webview; processing constraints verified. Physical self-echo test remains open. |
| Latency | Required marks and one log per turn verified; ≤900 ms live p50 remains unverified. |
| Coding dispatch | Real fixture child, confined path, JSONL, narration, completion and cancellation passed; authenticated CLI execution remains unverified. |
| Build quality | 86 Rust tests, all-target Clippy, formatting, JavaScript audio tests and production build passed in the continuation audit. |
| Packaging | NSIS generated/installed/launched; MSI generated; macOS/Linux configs and native CI supplied, not executed here. |
