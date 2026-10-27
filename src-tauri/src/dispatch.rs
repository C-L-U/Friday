use crate::{
    app_state::{InputEvent, Shared},
    llm::ToolCall,
};
use anyhow::{bail, ensure, Context};
use parking_lot::Mutex;
use process_wrap::tokio::{CommandWrap, KillOnDrop};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    ffi::OsString,
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::Command,
};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Serialize, Default)]
pub struct Status {
    pub running: bool,
    pub project: String,
    pub agent: String,
    pub detail: String,
}
struct Job {
    cancel: CancellationToken,
    status: Status,
}
#[derive(Default)]
pub struct CodingManager {
    running: Mutex<Option<Job>>,
    last: Mutex<Status>,
    sessions: Mutex<HashMap<(String, PathBuf), String>>,
    last_project: Mutex<Option<PathBuf>>,
}
impl CodingManager {
    pub fn status(&self) -> Status {
        self.running
            .lock()
            .as_ref()
            .map(|j| j.status.clone())
            .unwrap_or_else(|| self.last.lock().clone())
    }
    pub fn cancel(&self) -> bool {
        if let Some(job) = self.running.lock().as_ref() {
            job.cancel.cancel();
            true
        } else {
            false
        }
    }
}

pub fn slug(input: &str) -> String {
    let mut slug = String::new();
    for c in crate::turn::normalize(input).chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
        if slug.len() >= 64 {
            break;
        }
    }
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        return "proyecto".into();
    }
    let reserved = [
        "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
        "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
    ];
    if reserved.contains(&slug) {
        format!("proyecto-{slug}")
    } else {
        slug.into()
    }
}

fn project_dir(root: &str, name: &str) -> anyhow::Result<PathBuf> {
    let base = if let Some(rest) = root.strip_prefix("~/").or_else(|| root.strip_prefix("~\\")) {
        directories::BaseDirs::new()
            .context("No se encontró tu carpeta personal")?
            .home_dir()
            .join(rest)
    } else {
        PathBuf::from(root)
    };
    ensure!(
        base.is_absolute(),
        "La carpeta de proyectos debe ser absoluta"
    );
    std::fs::create_dir_all(&base)?;
    let base = std::fs::canonicalize(base)?;
    let path = base.join(slug(name));
    std::fs::create_dir_all(&path)?;
    let path = std::fs::canonicalize(path)?;
    ensure!(
        path.starts_with(&base) && path != base,
        "La carpeta sale del directorio de proyectos"
    );
    Ok(path)
}

struct Invocation {
    binary: PathBuf,
    prefix: Vec<OsString>,
}
fn invocation(path: PathBuf, agent: &str) -> Option<Invocation> {
    let is_shim = path
        .extension()
        .and_then(|s| s.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"));
    if !is_shim {
        return Some(Invocation {
            binary: path,
            prefix: vec![],
        });
    }
    // Run npm's JavaScript entrypoint directly. Never put a voice prompt through cmd.exe.
    let parent = path.parent()?;
    let entry = parent.join(if agent == "claude" {
        "node_modules/@anthropic-ai/claude-code/cli.js"
    } else {
        "node_modules/@openai/codex/bin/codex.js"
    });
    if !entry.is_file() {
        return None;
    }
    let node = if parent.join("node.exe").is_file() {
        parent.join("node.exe")
    } else {
        which::which("node").ok()?
    };
    Some(Invocation {
        binary: node,
        prefix: vec![entry.into_os_string()],
    })
}
fn discover(agent: &str) -> anyhow::Result<Invocation> {
    if let Ok(path) = which::which(agent) {
        if let Some(inv) = invocation(path, agent) {
            return Ok(inv);
        }
    }
    let mut dirs = vec![
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/opt/homebrew/bin"),
    ];
    if let Some(base) = directories::BaseDirs::new() {
        for suffix in [".local/bin", ".claude/local", ".npm-global/bin"] {
            dirs.push(base.home_dir().join(suffix));
        }
        dirs.push(base.config_dir().join("npm"));
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        dirs.push(PathBuf::from(local).join("Programs/OpenAI/Codex/bin"));
    }
    if let Some(appdata) = std::env::var_os("APPDATA") {
        dirs.push(PathBuf::from(appdata).join("npm"));
    }
    for dir in dirs {
        for suffix in ["", ".exe", ".cmd"] {
            let path = dir.join(format!("{agent}{suffix}"));
            if path.is_file() {
                if let Some(inv) = invocation(path, agent) {
                    return Ok(inv);
                }
            }
        }
    }
    bail!("No está instalado {agent}. Instálalo con npm install -g {} y abre {} una vez para iniciar sesión.",if agent=="claude"{"@anthropic-ai/claude-code"}else{"@openai/codex"},agent)
}

#[derive(Debug, PartialEq)]
pub enum CliEvent {
    Session(String),
    Progress(&'static str),
    Result { text: String, failed: bool },
}
pub fn parse_event(line: &str) -> Vec<CliEvent> {
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return vec![];
    };
    let mut events = Vec::new();
    match v["type"].as_str().unwrap_or("") {
        "system" if v["subtype"] == "init" => {
            if let Some(id) = v["session_id"].as_str() {
                events.push(CliEvent::Session(id.into()));
            }
        }
        "thread.started" => {
            if let Some(id) = v["thread_id"].as_str() {
                events.push(CliEvent::Session(id.into()));
            }
        }
        "assistant" => {
            if let Some(content) = v["message"]["content"].as_array() {
                for item in content {
                    if item["type"] == "tool_use" {
                        events.push(CliEvent::Progress(progress(
                            item["name"].as_str().unwrap_or(""),
                            item["input"]["command"].as_str().unwrap_or(""),
                        )));
                    }
                }
            }
        }
        "item.started" => {
            if v["item"]["type"] == "command_execution" {
                events.push(CliEvent::Progress(progress(
                    "Bash",
                    v["item"]["command"].as_str().unwrap_or(""),
                )));
            } else if v["item"]["type"] == "file_change" {
                events.push(CliEvent::Progress(
                    "Estoy creando la estructura del proyecto.",
                ));
            }
        }
        "item.completed" if v["item"]["type"] == "agent_message" => events.push(CliEvent::Result {
            text: v["item"]["text"].as_str().unwrap_or("").into(),
            failed: false,
        }),
        "result" => events.push(CliEvent::Result {
            text: v["result"]
                .as_str()
                .unwrap_or("El agente terminó sin un resumen.")
                .into(),
            failed: v["is_error"].as_bool().unwrap_or(false),
        }),
        "turn.failed" | "error" => events.push(CliEvent::Result {
            text: "El agente informó de un error. Revisa su instalación, sesión y permisos.".into(),
            failed: true,
        }),
        _ => {}
    }
    events
}
fn progress(tool: &str, command: &str) -> &'static str {
    let command = command.to_lowercase();
    if command.contains("install") || command.contains("cargo add") {
        "Instalando dependencias."
    } else if command.contains("test") || command.contains("check") {
        "Estoy comprobando que todo funcione."
    } else if command.contains("build") {
        "Estoy compilando el proyecto."
    } else if ["Write", "Edit", "MultiEdit"].contains(&tool) {
        "Estoy creando la estructura del proyecto."
    } else {
        "Sigo trabajando en el proyecto."
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RunArgs {
    task: String,
    project_name: String,
    #[serde(default)]
    agent: Option<String>,
}

pub async fn execute(state: &Shared, app: &tauri::AppHandle, tool: &ToolCall) -> Value {
    let result = match tool.function.name.as_str() {
        "run_coding_agent" => match serde_json::from_str::<RunArgs>(&tool.function.arguments) {
            Ok(args) => start(state.clone(), args).await,
            Err(_) => Err(anyhow::anyhow!("Argumentos de programación no válidos")),
        },
        "cancel_coding_agent" => Ok(json!({"cancelled":state.coding.cancel()})),
        "coding_agent_status" => Ok(json!(state.coding.status())),
        "open_project_folder" => open_folder(state, app).await,
        _ => Err(anyhow::anyhow!("Herramienta desconocida")),
    };
    match result {
        Ok(value) => value,
        Err(error) => json!({"error":error.to_string()}),
    }
}

async fn open_folder(state: &Shared, app: &tauri::AppHandle) -> anyhow::Result<Value> {
    use tauri_plugin_opener::OpenerExt;
    let path = state
        .coding
        .last_project
        .lock()
        .clone()
        .context("Todavía no hay un proyecto")?;
    let app = app.clone();
    tokio::task::spawn_blocking(move || {
        app.opener()
            .open_path(path.to_string_lossy().into_owned(), None::<&str>)
    })
    .await??;
    Ok(json!({"opened":true}))
}

async fn start(state: Shared, args: RunArgs) -> anyhow::Result<Value> {
    ensure!(
        !args.task.trim().is_empty() && args.task.len() <= 32000,
        "La tarea no es válida"
    );
    let config = state.config.current.read().coding.clone();
    let agent = args.agent.unwrap_or_else(|| config.default_agent.clone());
    ensure!(
        ["claude", "codex"].contains(&agent.as_str()),
        "Agente no válido"
    );
    validate_extra_args(&config.extra_args)?;
    if state.coding.running.lock().is_some() {
        return Ok(
            json!({"started":false,"message":"Ya hay un proyecto en marcha. Puedes cancelarlo o esperar a que termine."}),
        );
    }
    let root = config.projects_dir.clone();
    let name = args.project_name.clone();
    let agent_copy = agent.clone();
    let mock = state.mock;
    let (path, invocation) = tokio::task::spawn_blocking(move || {
        let invocation = if mock {
            Invocation {
                binary: std::env::current_exe()?,
                prefix: vec!["--mock-coding-agent".into()],
            }
        } else {
            discover(&agent_copy)?
        };
        Ok::<_, anyhow::Error>((project_dir(&root, &name)?, invocation))
    })
    .await??;
    let cancel = state.shutdown.child_token();
    {
        let mut running = state.coding.running.lock();
        ensure!(running.is_none(), "Ya hay un proyecto en marcha");
        *running = Some(Job {
            cancel: cancel.clone(),
            status: Status {
                running: true,
                project: args.project_name.clone(),
                agent: agent.clone(),
                detail: "Iniciando".into(),
            },
        });
    }
    *state.coding.last_project.lock() = Some(path.clone());
    let response = json!({"started":true,"project":args.project_name,"agent":agent});
    crate::diagnostics::spawn("dispatch", async move {
        let result = tokio::time::timeout(
            Duration::from_secs(config.timeout_minutes * 60),
            run_process(
                &state,
                &path,
                &agent,
                &args.task,
                &config.extra_args,
                invocation,
                cancel.clone(),
            ),
        )
        .await;
        let (detail, summary) = if cancel.is_cancelled() {
            ("Cancelado".to_owned(), None)
        } else {
            match result {
                Ok(Ok(text)) => ("Terminado".into(), Some((text, true))),
                Ok(Err(error)) => (
                    "Error".into(),
                    Some((format!("No pude terminar el proyecto: {error}"), false)),
                ),
                Err(_) => {
                    cancel.cancel();
                    (
                        "Tiempo límite agotado".into(),
                        Some((
                            "El proyecto alcanzó el tiempo límite y detuve el agente.".into(),
                            false,
                        )),
                    )
                }
            }
        };
        if let Some(mut job) = state.coding.running.lock().take() {
            job.status.running = false;
            job.status.detail = detail;
            *state.coding.last.lock() = job.status;
        }
        if let Some((text, summarize)) = summary {
            let _ = state
                .event_tx
                .send(InputEvent::Background { text, summarize })
                .await;
        }
    });
    Ok(response)
}

fn validate_extra_args(args: &[String]) -> anyhow::Result<()> {
    for arg in args {
        let flag = arg.split('=').next().unwrap_or("");
        ensure!(
            ![
                "-C",
                "--cd",
                "--cwd",
                "--add-dir",
                "--worktree",
                "-w",
                "--output-format",
                "--input-format",
                "--resume",
                "-r",
                "--continue",
                "--json"
            ]
            .contains(&flag),
            "El argumento {flag} cambia la carpeta o el protocolo del agente"
        );
    }
    Ok(())
}

async fn run_process(
    state: &Shared,
    path: &Path,
    agent: &str,
    task: &str,
    extra: &[String],
    invocation: Invocation,
    cancel: CancellationToken,
) -> anyhow::Result<String> {
    if !tokio::fs::try_exists(path.join(".git")).await? {
        let git = tokio::task::spawn_blocking(|| which::which("git").ok()).await?;
        if let Some(git) = git {
            let mut cmd = Command::new(git);
            cmd.arg("init")
                .current_dir(path)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true);
            #[cfg(windows)]
            cmd.creation_flags(0x08000000);
            tokio::select! {_=cancel.cancelled()=>return Ok(String::new()),result=tokio::time::timeout(Duration::from_secs(10),cmd.status())=>{let _=result?;}}
        }
    }
    let session = state
        .coding
        .sessions
        .lock()
        .get(&(agent.into(), path.into()))
        .cloned();
    let mut command = Command::new(invocation.binary);
    command
        .args(invocation.prefix)
        .current_dir(path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if !state.mock {
        if agent == "claude" {
            command.args([
                "-p",
                "--output-format",
                "stream-json",
                "--verbose",
                "--permission-mode",
                "acceptEdits",
            ]);
            if let Some(session) = &session {
                command.args(["--resume", session]);
            }
        } else {
            command.args([
                "exec",
                "--json",
                "--sandbox",
                "workspace-write",
                "--skip-git-repo-check",
            ]);
            if let Some(session) = &session {
                command.args(["resume", session]);
            }
            command.arg("-");
        }
        command.args(extra);
    }
    // Voice keys stay inside Jarvis, even if the developer shell had exported them.
    command
        .env_remove("DEEPGRAM_API_KEY")
        .env_remove("XAI_API_KEY");
    let mut command = CommandWrap::from(command);
    command.wrap(KillOnDrop);
    #[cfg(unix)]
    command.wrap(process_wrap::tokio::ProcessGroup::leader());
    #[cfg(windows)]
    {
        command.wrap(process_wrap::tokio::CreationFlags(
            windows::Win32::System::Threading::CREATE_NO_WINDOW,
        ));
        command.wrap(process_wrap::tokio::JobObject);
    }
    let mut child = command.spawn().context("No se pudo iniciar el agente")?;
    let mut stdin = child.stdin().take().context("Agent stdin missing")?;
    let stdout = child.stdout().take().context("Agent stdout missing")?;
    let stderr = child.stderr().take().context("Agent stderr missing")?;
    tokio::select! {_=cancel.cancelled()=>{std::pin::Pin::from(child.kill()).await?;return Ok(String::new());},result=stdin.write_all(task.as_bytes())=>result?}
    drop(stdin);
    let mut stdout = BufReader::new(stdout);
    let mut stderr = BufReader::new(stderr);
    let mut out_open = true;
    let mut err_open = true;
    let mut result = String::new();
    let mut failed = false;
    let mut last_progress = None::<Instant>;
    let mut out_buffer = Vec::new();
    let mut err_buffer = Vec::new();
    while out_open || err_open {
        tokio::select! {
            _=cancel.cancelled()=>{std::pin::Pin::from(child.kill()).await?;return Ok(String::new());},
            line=read_line(&mut stdout,&mut out_buffer),if out_open=>match line?{
                Some(line)=>for event in parse_event(&line){match event{
                    CliEvent::Session(id)=>{state.coding.sessions.lock().insert((agent.into(),path.into()),id);}
                    CliEvent::Progress(text)=>{
                        if let Some(job)=state.coding.running.lock().as_mut(){job.status.detail=text.into();}
                        if last_progress.is_none_or(|time|time.elapsed()>=Duration::from_secs(12)){
                            last_progress=Some(Instant::now());let _=state.event_tx.send(InputEvent::Background{text:text.into(),summarize:false}).await;
                        }
                    }
                    CliEvent::Result{text,failed:is_error}=>{result=text.chars().take(8000).collect();failed|=is_error;}
                }},
                None=>out_open=false,
            },
            line=read_line(&mut stderr,&mut err_buffer),if err_open=>match line?{
                Some(line)=>{
                    let keys=state.keys.read();let mut redacted=line;
                    for key in [&keys.deepgram,&keys.xai]{if !key.is_empty(){redacted=redacted.replace(key,"[redacted]");}}
                    tracing::debug!(agent,line=%redacted,"coding stderr");
                }
                None=>err_open=false,
            }
        }
    }
    let status = tokio::select! {_=cancel.cancelled()=>{std::pin::Pin::from(child.kill()).await?;return Ok(String::new());},status=child.wait()=>status?};
    ensure!(
        status.success() && !failed,
        "El agente terminó con un error. Revisa su sesión y permisos"
    );
    if result.is_empty() {
        result = "El agente terminó correctamente sin un resumen de texto.".into();
    }
    Ok(result)
}

async fn read_line<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
    bytes: &mut Vec<u8>,
) -> anyhow::Result<Option<String>> {
    // The outer select may cancel this future: use fill_buf/consume rather than
    // read_line so consumed data is not lost on interleaved stdout/stderr.
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return if bytes.is_empty() {
                Ok(None)
            } else {
                Ok(Some(
                    String::from_utf8_lossy(&std::mem::take(bytes)).into_owned(),
                ))
            };
        }
        let count = available
            .iter()
            .position(|b| *b == b'\n')
            .map_or(available.len(), |i| i + 1);
        let ended = available[count - 1] == b'\n';
        bytes.extend_from_slice(&available[..count]);
        reader.consume(count);
        ensure!(
            bytes.len() <= 2 * 1024 * 1024,
            "Agent output line exceeded two megabytes"
        );
        if ended {
            return Ok(Some(
                String::from_utf8_lossy(&std::mem::take(bytes))
                    .trim_end()
                    .into(),
            ));
        }
    }
}

pub fn mock_cli_main() -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let mut task = String::new();
        tokio::io::stdin().read_to_string(&mut task).await?;
        tokio::fs::write(
            "demo.txt",
            "Jarvis offline subprocess fixture. No coding provider was contacted.\n",
        )
        .await?;
        for line in include_str!("../tests/fixtures/claude.jsonl").lines() {
            println!("{line}");
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn claude_fixture() {
        let events: Vec<_> = include_str!("../tests/fixtures/claude.jsonl")
            .lines()
            .flat_map(parse_event)
            .collect();
        assert_eq!(events.len(), 5);
        assert_eq!(events[0], CliEvent::Session("fixture-session".into()));
        assert!(events.contains(&CliEvent::Progress("Instalando dependencias.")));
        assert!(matches!(
            events.last(),
            Some(CliEvent::Result { failed: false, .. })
        ));
    }
    #[test]
    fn codex_fixture() {
        let events: Vec<_> = include_str!("../tests/fixtures/codex.jsonl")
            .lines()
            .flat_map(parse_event)
            .collect();
        assert_eq!(events.len(), 3);
        assert_eq!(events[0], CliEvent::Session("fixture-codex-session".into()));
    }
    #[test]
    fn garbage_is_ignored() {
        assert!(parse_event("garbage").is_empty());
        assert!(parse_event("{}").is_empty());
    }
    #[test]
    fn slugs_cannot_traverse() {
        for name in [
            "../../secret",
            "C:\\Windows",
            "/tmp",
            "..",
            "CON",
            "ñ á demo",
            "a:b",
        ] {
            let s = slug(name);
            assert!(!s.is_empty());
            assert!(s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
            assert_ne!(s, "con");
        }
        assert_eq!(slug("Mi aplicación"), "mi-aplicacion");
    }
    #[test]
    fn protocol_args_cannot_override() {
        assert!(validate_extra_args(&["--cd=/tmp".into()]).is_err());
        assert!(validate_extra_args(&["--dangerously-skip-permissions".into()]).is_ok());
    }
    #[tokio::test]
    async fn bounded_line_reader() {
        let mut input = BufReader::new(&b"first\nsecond\nlast"[..]);
        let mut buf = Vec::new();
        assert_eq!(
            read_line(&mut input, &mut buf).await.unwrap().as_deref(),
            Some("first")
        );
        assert_eq!(
            read_line(&mut input, &mut buf).await.unwrap().as_deref(),
            Some("second")
        );
        assert_eq!(
            read_line(&mut input, &mut buf).await.unwrap().as_deref(),
            Some("last")
        );
        assert!(read_line(&mut input, &mut buf).await.unwrap().is_none());
    }
    #[tokio::test]
    async fn cancelled_read_preserves_partial_json() {
        let (mut writer, reader) = tokio::io::duplex(1024);
        let mut reader = BufReader::new(reader);
        let mut bytes = Vec::new();
        writer.write_all(b"{\"type\":").await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(5), read_line(&mut reader, &mut bytes))
                .await
                .is_err()
        );
        writer.write_all(b"\"system\"}\n").await.unwrap();
        assert_eq!(
            read_line(&mut reader, &mut bytes).await.unwrap().as_deref(),
            Some("{\"type\":\"system\"}")
        );
    }
    #[test]
    fn project_paths_are_confined() {
        let root = tempfile::tempdir().unwrap();
        let project = project_dir(root.path().to_str().unwrap(), "../../outside").unwrap();
        assert!(project.starts_with(root.path().canonicalize().unwrap()));
        assert_eq!(project.file_name().unwrap(), "outside");
        assert!(project_dir("relative/path", "demo").is_err());
    }
    #[cfg(unix)]
    #[test]
    fn project_symlink_cannot_escape() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("demo")).unwrap();
        assert!(project_dir(root.path().to_str().unwrap(), "demo").is_err());
    }
}
