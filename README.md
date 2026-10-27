# Jarvis

Asistente de voz de escritorio en Rust y Tauri 2. Una esfera flotante, conversación en español, interrupciones y delegación a Claude Code o Codex. HTML, CSS y JavaScript nativo, sin npm ni empaquetador de frontend.

## Uso

Instala el paquete de tu plataforma y abre Jarvis. La primera vez aparecen los ajustes: introduce una clave de Deepgram y otra de xAI y pulsa **Guardar**. Jarvis valida ambas, las guarda en el llavero del sistema y activa el micrófono. Di «Jarvis, hola». Durante los veinte segundos posteriores a una respuesta puedes continuar sin repetir su nombre.

La esfera se arrastra; el engranaje aparece al pasar el cursor. Escape cierra los ajustes. Doble clic oculta la ventana; el icono de bandeja la vuelve a mostrar. `Ctrl+Shift+J` / `Cmd+Shift+J` silencia el micrófono. En siguientes arranques escucha desde la bandeja, sin saludo. El inicio de sesión automático se habilita en la aplicación instalada, no durante desarrollo ni en modo de prueba.

Las claves guardadas nunca se devuelven a JavaScript. Si el llavero no está disponible se usa `credentials.json` con permisos exclusivos del usuario: 0600 en Unix y ACL restringida en Windows. Las entradas de contraseña se vacían al guardar. Si una validación falla, la clave válida se conserva en memoria mientras corriges la otra.

## Desarrollo y compilación

Instala Rust estable y los [prerrequisitos oficiales de Tauri](https://v2.tauri.app/start/prerequisites/): Visual Studio Build Tools con C++/Windows SDK en Windows; Xcode Command Line Tools en macOS; WebKitGTK 4.1, AppIndicator y las bibliotecas indicadas en el workflow en Linux. Después:

```sh
cargo install tauri-cli --locked --version '^2'
cargo tauri dev
```

Para generar instaladores en cada sistema nativo:

```sh
cargo tauri build
```

Los paquetes quedan en `target/release/bundle/`: Windows NSIS `.exe` y `.msi`; macOS `.app` y `.dmg`; Linux `.AppImage` y `.deb`. Windows incorpora el instalador de WebView2; Linux empaqueta las bibliotecas multimedia en AppImage. El workflow `.github/workflows/build.yml` compila Windows, Linux y macOS Apple Silicon/Intel y conserva artefactos. No publica releases automáticamente. Los certificados de firma y la notarización de distribución requieren las credenciales del editor; los paquetes locales no están firmados.

En el equipo de desarrollo de esta entrega se usó MinGW porque no hay linker/SDK de Visual Studio. Su instalación en una ruta con espacios falla internamente. La comprobación local se ejecutó así, sin modificar las rutas del proyecto:

```powershell
$env:PATH = 'C:/mingw64/bin;' + $env:PATH
$env:CARGO_TARGET_DIR = 'C:/tmp/jarvis-target'
cargo tauri dev
cargo tauri build
```

Estas dos variables son un arreglo para ese equipo, no un requisito del repositorio ni del instalador.

## Configuración y datos

`config.toml` y `system_prompt.txt` se crean en el directorio de configuración de `com.jarvis.voice` de la plataforma. Los valores base están en `src-tauri/defaults/`. Las modificaciones válidas se recargan automáticamente; el prompt se guarda 600 ms después de editarlo y se usa en el siguiente turno. **Restaurar predeterminado** recupera el texto español original. Un TOML inválido conserva la última configuración válida.

La configuración incluye modelos, URLs, velocidad, idioma, activación, historial, directorio de proyectos y atajo. `tts.transport` admite `http` y `websocket`. La conversación, PCM, historial y sesiones de agentes se mantienen en memoria; los propios agentes externos pueden conservar sus sesiones y proyectos según su configuración. Jarvis no graba audio ni conversaciones en disco. El VAD decide cuándo conectar Deepgram: el nombre de activación se reconoce sobre la transcripción, por lo que el audio detectado como voz se envía a Deepgram aunque todavía no hayas dicho «Jarvis».

## Agentes de programación

Di «Jarvis, crea una app demo de…», «cómo va el proyecto» o «cancela eso». El agente trabaja en `~/Jarvis/projects/<nombre>`; los nombres se normalizan, la ruta se canonicaliza y se verifica que quede dentro de la raíz. Esto limita la carpeta de trabajo, **no constituye un sandbox del sistema operativo**. Codex usa además `workspace-write`; Claude conserva su política de permisos `acceptEdits`.

Claude Code o Codex deben estar instalados y autenticados en el equipo para esta función. Las dos claves de voz no autentican sus CLI. Si faltan, Jarvis lo explica por voz. Descubre binarios nativos y shims de npm sin interpolar la tarea en un shell. Envía la tarea por stdin, interpreta JSONL y conserva el identificador de sesión por proyecto durante la ejecución de Jarvis. Las siguientes solicitudes al mismo proyecto reanudan la sesión. El agente funciona en segundo plano, con cancelación del árbol de procesos y un límite predeterminado de treinta minutos. Los avisos usan plantillas, esperan a que dejes de hablar y se limitan a uno cada doce segundos; el resumen final usa una sola llamada a Grok. Puedes aceptar por voz la oferta de abrir la carpeta.

`coding.extra_args` permite opciones de la CLI; se rechazan las que cambian carpeta o protocolo. `--dangerously-skip-permissions` es exclusivamente opt-in y sigue ejecutándose dentro de la carpeta del proyecto. Las tareas externas pueden requerir permisos adicionales: no se autorizan operaciones fuera de la política del agente de forma automática.

## Verificación

```sh
cargo fmt --all -- --check
cargo check
cargo clippy -- -D warnings
cargo test
node scripts/test-audio.mjs
node scripts/test-playback.mjs
```

Para comprobar explícitamente el llavero nativo sin tocar las credenciales de Jarvis: `cargo run --example keyring_probe`. Crea una entrada sintética única, la lee y la elimina.

Las pruebas de Rust también ejercitan los clientes de producción contra servidores HTTP/WebSocket locales: recepción incremental antes del fin de la respuesta, parámetros opcionales, llamadas a herramientas incompletas, alineación PCM y cierre de STT. No requieren claves ni contactan a los proveedores.

Node sólo se usa para pruebas, nunca para construir o ejecutar Jarvis. El modo offline selecciona implementaciones de los traits STT/LLM/TTS:

```powershell
$env:JARVIS_MOCK = '1'
cargo tauri dev
```

En macOS/Linux: `JARVIS_MOCK=1 cargo tauri dev`. Usa configuración separada en `mock/`, no consulta ni modifica el llavero y no requiere micrófono. Su TTS emite un tono sintético; no imita una validación de los proveedores. El subproceso de prueba escribe `demo.txt` y emite fixtures JSONL, pasando por el mismo gestor de procesos que las CLI reales.

Para la prueba integral en Windows, inicia con `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=9222` y `JARVIS_MOCK=1`; ejecuta `node scripts/smoke-webview.mjs`. Comprueba audio, ajustes, prompt, cancelación, historial, herramientas y proceso real de prueba. `node scripts/smoke-first-run.mjs --microphone` comprueba el primer arranque sin modo mock ni claves guardadas y opcionalmente cincuenta frames del micrófono real, sin enviar audio a los proveedores. No habilites el puerto de depuración en distribución normal.

`debug_latency` devuelve los últimos veinte turnos. Cada turno produce una línea `turn_latency` con sus marcas. Al recibir el endpoint de Deepgram, `speech_end` toma la hora del último frame que el VAD local clasificó como voz: `final_transcript` incluye así la espera de endpointing y transporte. Es una estimación por frames de 20 ms, sensible al ruido; para medir el final acústico exacto hace falta loopback. `first_audio_played` es la hora programada en Web Audio más su latencia de salida informada, no una medición física. El filtro predeterminado de interrupción exige 200 ms de voz sostenida; no puede garantizar detenerse a los 150 ms desde el comienzo de esa misma voz. Los detalles y límites de las pruebas están en [NOTES.md](NOTES.md).
