use async_trait::async_trait;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("LLM request failed: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("Invalid LLM stream: {0}")]
    Protocol(String),
    #[error("LLM provider returned HTTP {0}")]
    Provider(u16),
    #[error("LLM first token timeout (8 s)")]
    Timeout,
    #[error("Turn cancelled")]
    Cancelled,
}

/// SSE decoding stays byte-oriented until a complete line is present, so UTF-8
/// codepoints split across TCP chunks never become replacement characters.
#[derive(Default)]
pub struct SseDecoder {
    line: Vec<u8>,
    data: Vec<String>,
    cr: bool,
    event_bytes: usize,
}
impl SseDecoder {
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<String>, LlmError> {
        let mut events = Vec::new();
        for &b in bytes {
            if b == b'\n' && self.cr {
                self.cr = false;
                continue;
            }
            self.cr = b == b'\r';
            if b == b'\n' || b == b'\r' {
                self.line(&mut events)?;
            } else {
                self.line.push(b);
                if self.line.len() > 1024 * 1024 {
                    return Err(LlmError::Protocol("oversized SSE line".into()));
                }
            }
        }
        Ok(events)
    }
    fn line(&mut self, output: &mut Vec<String>) -> Result<(), LlmError> {
        let line = String::from_utf8(std::mem::take(&mut self.line))
            .map_err(|_| LlmError::Protocol("invalid UTF-8".into()))?;
        if line.is_empty() {
            if !self.data.is_empty() {
                output.push(self.data.join("\n"));
                self.data.clear();
                self.event_bytes = 0;
            }
        } else if let Some(data) = line.strip_prefix("data:") {
            let data = data.strip_prefix(' ').unwrap_or(data);
            self.event_bytes += data.len();
            if self.event_bytes > 2 * 1024 * 1024 {
                return Err(LlmError::Protocol("oversized SSE event".into()));
            }
            self.data.push(data.into());
        }
        Ok(())
    }
    pub fn finish(&mut self) -> Result<Vec<String>, LlmError> {
        let mut output = Vec::new();
        self.line(&mut output)?;
        if !self.data.is_empty() {
            output.push(self.data.join("\n"));
            self.data.clear();
        }
        Ok(output)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Function {
    pub name: String,
    pub arguments: String,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub function: Function,
}
#[derive(Default)]
pub struct ToolAssembler(BTreeMap<u64, ToolCall>);
impl ToolAssembler {
    pub fn push(&mut self, deltas: &Value) -> Result<(), LlmError> {
        let Some(deltas) = deltas.as_array() else {
            return Ok(());
        };
        for delta in deltas {
            let index = delta["index"]
                .as_u64()
                .ok_or_else(|| LlmError::Protocol("missing tool index".into()))?;
            if index >= 8 {
                return Err(LlmError::Protocol("too many tools".into()));
            }
            let tool = self.0.entry(index).or_insert_with(|| ToolCall {
                kind: "function".into(),
                ..Default::default()
            });
            if let Some(id) = delta["id"].as_str() {
                if tool.id != id {
                    tool.id.push_str(id);
                }
            }
            if let Some(name) = delta["function"]["name"].as_str() {
                tool.function.name.push_str(name);
            }
            if let Some(args) = delta["function"]["arguments"].as_str() {
                tool.function.arguments.push_str(args);
            }
            if tool.function.arguments.len() > 64000 {
                return Err(LlmError::Protocol("oversized tool arguments".into()));
            }
        }
        Ok(())
    }
    pub fn finish(self) -> Result<Vec<ToolCall>, LlmError> {
        let mut tools = Vec::new();
        for tool in self.0.into_values() {
            if tool.id.is_empty()
                || tool.function.name.is_empty()
                || serde_json::from_str::<Value>(&tool.function.arguments).is_err()
            {
                return Err(LlmError::Protocol("incomplete tool call".into()));
            }
            tools.push(tool);
        }
        Ok(tools)
    }
}

pub fn tools() -> Value {
    json!([
        {"type":"function","function":{"name":"run_coding_agent","description":"Construye o modifica un proyecto en segundo plano. Di primero una breve confirmación en voz alta.","parameters":{"type":"object","properties":{"task":{"type":"string"},"agent":{"type":"string","enum":["claude","codex"]},"project_name":{"type":"string"}},"required":["task","project_name"],"additionalProperties":false}}},
        {"type":"function","function":{"name":"cancel_coding_agent","description":"Cancela el trabajo de programación actual.","parameters":{"type":"object","properties":{},"additionalProperties":false}}},
        {"type":"function","function":{"name":"coding_agent_status","description":"Consulta el progreso del proyecto actual.","parameters":{"type":"object","properties":{},"additionalProperties":false}}},
        {"type":"function","function":{"name":"open_project_folder","description":"Abre la carpeta del último proyecto únicamente cuando el usuario lo pide o acepta la oferta.","parameters":{"type":"object","properties":{},"additionalProperties":false}}}
    ])
}

pub struct LlmRequest {
    pub config: crate::config::Llm,
    pub key: zeroize::Zeroizing<String>,
    pub messages: Vec<Value>,
    pub allow_tools: bool,
}
pub struct LlmResponse {
    pub text: String,
    pub tools: Vec<ToolCall>,
}
#[async_trait]
pub trait LlmBackend: Send + Sync {
    async fn stream(
        &self,
        request: LlmRequest,
        output: mpsc::Sender<String>,
        cancel: CancellationToken,
    ) -> Result<LlmResponse, LlmError>;
}
pub struct Grok {
    pub client: reqwest::Client,
    pub omit_optional: Arc<AtomicBool>,
}
pub struct MockLlm;

impl Grok {
    async fn stream_once(
        &self,
        request: LlmRequest,
        output: mpsc::Sender<String>,
        cancel: CancellationToken,
    ) -> Result<LlmResponse, LlmError> {
        let mut body = json!({"model":request.config.model,"messages":request.messages,"stream":true,"max_tokens":request.config.max_tokens,"tools":tools()});
        if !request.allow_tools {
            if let Some(body) = body.as_object_mut() {
                body.remove("tools");
            }
        }
        let optional = !self.omit_optional.load(Ordering::Relaxed)
            && !request.config.reasoning_effort.is_empty();
        if optional {
            body["reasoning_effort"] = json!(request.config.reasoning_effort);
        }
        let url = format!(
            "{}/chat/completions",
            request.config.base_url.trim_end_matches('/')
        );
        let response = tokio::select! { _=cancel.cancelled()=>return Err(LlmError::Cancelled), result=self.client.post(&url).bearer_auth(request.key.as_str()).json(&body).send()=>result? };
        crate::diagnostics::record("llm", &format!("HTTP {}", response.status().as_u16()), None);
        if response.status().is_client_error() && optional {
            self.omit_optional.store(true, Ordering::Relaxed);
        }
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let detail = crate::diagnostics::provider_error(response).await;
            crate::diagnostics::record("llm", &detail, None);
            return Err(LlmError::Provider(status));
        }
        let mut stream = response.bytes_stream();
        let mut decoder = SseDecoder::default();
        let mut text = String::new();
        let mut calls = ToolAssembler::default();
        let mut finish_reason = None;
        let mut done = false;
        while !done {
            let bytes = tokio::select! { _=cancel.cancelled()=>return Err(LlmError::Cancelled), bytes=stream.next()=>bytes };
            let events = match bytes {
                Some(bytes) => decoder.push(&bytes?)?,
                None => {
                    done = true;
                    decoder.finish()?
                }
            };
            for event in events {
                if event.trim() == "[DONE]" {
                    done = true;
                    break;
                }
                let value: Value =
                    serde_json::from_str(&event).map_err(|e| LlmError::Protocol(e.to_string()))?;
                if value.get("error").is_some() {
                    return Err(LlmError::Protocol("provider stream error".into()));
                }
                let delta = &value["choices"][0]["delta"];
                if let Some(content) = delta["content"].as_str() {
                    if !content.is_empty() {
                        if text.is_empty() {
                            crate::diagnostics::record("llm_token", "first token", None);
                        }
                        text.push_str(content);
                        tokio::select! { _=cancel.cancelled()=>return Err(LlmError::Cancelled), result=output.send(content.to_owned())=>result.map_err(|_|LlmError::Cancelled)? }
                    }
                }
                calls.push(&delta["tool_calls"])?;
                if let Some(reason) = value["choices"][0]["finish_reason"].as_str() {
                    finish_reason = Some(reason.to_owned());
                }
            }
        }
        // A valid JSON argument object can arrive before the model has finished
        // deciding its tool call. Never execute it after a truncated response.
        let tools = calls.finish()?;
        if text.trim().is_empty() && tools.is_empty() {
            return Err(LlmError::Protocol(
                "LLM returned no text or tool calls".into(),
            ));
        }
        match finish_reason.as_deref() {
            Some("tool_calls") if !tools.is_empty() => {}
            Some("stop" | "length") if tools.is_empty() => {}
            _ => {
                return Err(LlmError::Protocol(
                    "stream ended without a complete response".into(),
                ))
            }
        }
        Ok(LlmResponse { text, tools })
    }
}

#[async_trait]
impl LlmBackend for Grok {
    async fn stream(
        &self,
        request: LlmRequest,
        output: mpsc::Sender<String>,
        cancel: CancellationToken,
    ) -> Result<LlmResponse, LlmError> {
        for attempt in 0..2 {
            let optional = !self.omit_optional.load(Ordering::Relaxed)
                && !request.config.reasoning_effort.is_empty();
            let copy = LlmRequest {
                config: request.config.clone(),
                key: zeroize::Zeroizing::new(request.key.to_string()),
                messages: request.messages.clone(),
                allow_tools: request.allow_tools,
            };
            let (tx, mut rx) = mpsc::channel(32);
            let future = self.stream_once(copy, tx, cancel.clone());
            tokio::pin!(future);
            let deadline = tokio::time::sleep(std::time::Duration::from_secs(8));
            tokio::pin!(deadline);
            let mut delivered = false;
            let result = loop {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return Err(LlmError::Cancelled),
                    Some(token) = rx.recv() => {
                        delivered = true;
                        tokio::select! { _ = cancel.cancelled() => return Err(LlmError::Cancelled), result = output.send(token) => result.map_err(|_| LlmError::Cancelled)? }
                    },
                    result = &mut future => break result,
                    _ = &mut deadline, if !delivered => break Err(LlmError::Timeout),
                }
            };
            while let Ok(token) = rx.try_recv() {
                delivered = true;
                tokio::select! { _ = cancel.cancelled() => return Err(LlmError::Cancelled), result = output.send(token) => result.map_err(|_| LlmError::Cancelled)? }
            }
            match result {
                Err(error)
                    if attempt == 0
                        && !delivered
                        && (matches!(
                            &error,
                            LlmError::Timeout
                                | LlmError::Transport(_)
                                | LlmError::Provider(500..=599)
                        ) || (optional && matches!(&error, LlmError::Provider(400..=499)))) =>
                {
                    crate::diagnostics::record("llm_retry", &format!("{error}; retry 1/1"), None);
                }
                result => return result,
            }
        }
        Err(LlmError::Protocol("LLM retry budget exhausted".into()))
    }
}

#[async_trait]
impl LlmBackend for MockLlm {
    async fn stream(
        &self,
        request: LlmRequest,
        output: mpsc::Sender<String>,
        cancel: CancellationToken,
    ) -> Result<LlmResponse, LlmError> {
        let user = request
            .messages
            .iter()
            .rev()
            .find(|m| m["role"] == "user")
            .and_then(|m| m["content"].as_str())
            .unwrap_or("")
            .to_lowercase();
        let has_result = request.messages.last().is_some_and(|m| m["role"] == "tool");
        let (text, tools) = if request.allow_tools
            && !has_result
            && (user.contains("crea") || user.contains("construye"))
        {
            (
                "Perfecto, me pongo con eso ahora mismo.",
                vec![ToolCall {
                    id: "mock-build".into(),
                    kind: "function".into(),
                    function: Function {
                        name: "run_coding_agent".into(),
                        arguments: json!({"task":user,"project_name":"demo","agent":"claude"})
                            .to_string(),
                    },
                }],
            )
        } else if request.allow_tools && !has_result && user.contains("cancela") {
            (
                "Claro, detengo el proyecto.",
                vec![ToolCall {
                    id: "mock-cancel".into(),
                    kind: "function".into(),
                    function: Function {
                        name: "cancel_coding_agent".into(),
                        arguments: "{}".into(),
                    },
                }],
            )
        } else if !request.allow_tools {
            (
                "La prueba del proyecto terminó correctamente. ¿Quieres que abra la carpeta?",
                vec![],
            )
        } else {
            ("Claro, estoy aquí. Podemos hablar con calma y puedes interrumpirme cuando quieras.",vec![])
        };
        for token in text.split_inclusive(' ') {
            tokio::select! { _=cancel.cancelled()=>return Err(LlmError::Cancelled), _=tokio::time::sleep(std::time::Duration::from_millis(18))=>{} }
            output
                .send(token.into())
                .await
                .map_err(|_| LlmError::Cancelled)?;
        }
        Ok(LlmResponse {
            text: text.into(),
            tools,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{self, Response};

    fn request(url: String) -> LlmRequest {
        let mut config = crate::config::Config::parse("").unwrap().llm;
        config.base_url = url;
        LlmRequest {
            config,
            key: zeroize::Zeroizing::new("fixture-key".into()),
            messages: vec![json!({"role":"user","content":"Hola"})],
            allow_tools: true,
        }
    }

    fn sse(delta: Value, finish: Option<&str>) -> String {
        format!(
            "data: {}\n\n",
            json!({"choices":[{"delta":delta,"finish_reason":finish}]})
        )
    }

    #[tokio::test]
    async fn production_http_streams_before_response_ends() {
        let (release, gate) = tokio::sync::oneshot::channel();
        let mut fixture = Response::ok(
            (sse(json!({"content":" ¿Qué tal?"}), None)
                + &sse(json!({}), Some("stop"))
                + "data: [DONE]\n\n")
                .as_bytes(),
        );
        fixture
            .chunks
            .insert(0, sse(json!({"content":"Hola."}), None).into_bytes());
        fixture.after_first = Some(gate);
        let (url, server) = test_support::http(vec![fixture]).await;
        let backend = Grok {
            client: test_support::client(),
            omit_optional: Arc::new(AtomicBool::new(false)),
        };
        let (tx, mut rx) = mpsc::channel(32);
        let task = tokio::spawn(async move {
            backend
                .stream(request(url), tx, CancellationToken::new())
                .await
        });
        let first = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first, "Hola.");
        assert!(!task.is_finished(), "first token precedes HTTP completion");
        release.send(()).unwrap();
        let reply = task.await.unwrap().unwrap();
        assert_eq!(reply.text, "Hola. ¿Qué tal?");
        assert!(reply.tools.is_empty());
        let captured = server.await.unwrap();
        assert!(captured[0].headers.starts_with("POST /chat/completions "));
        assert!(captured[0]
            .headers
            .to_lowercase()
            .contains("authorization: bearer fixture-key"));
        assert_eq!(captured[0].body["stream"], true);
    }

    #[tokio::test]
    async fn production_http_never_dispatches_unconfirmed_tools() {
        for reason in [None, Some("length"), Some("stop"), Some("tool_calls")] {
            for done in ["", "data: [DONE]\n\n"] {
                let body = sse(
                    json!({"tool_calls":[{"index":0,"id":"call-1","function":{"name":"coding_agent_status","arguments":"{}"}}]}),
                    None,
                ) + &sse(json!({}), reason)
                    + done;
                let (url, server) = test_support::http(vec![Response::ok(body.as_bytes())]).await;
                let backend = Grok {
                    client: test_support::client(),
                    omit_optional: Arc::new(AtomicBool::new(false)),
                };
                let (tx, _rx) = mpsc::channel(32);
                let result = backend
                    .stream(request(url), tx, CancellationToken::new())
                    .await;
                assert_eq!(
                    result.is_ok(),
                    reason == Some("tool_calls"),
                    "reason={reason:?}, done={done:?}"
                );
                server.await.unwrap();
            }
        }
    }

    #[tokio::test]
    async fn production_http_retries_optional_parameters_once_per_session() {
        let body = sse(json!({"content":"Listo."}), Some("stop")) + "data: [DONE]\n\n";
        let mut rejected = Response::ok(b"{}");
        rejected.status = 400;
        let (url, server) = test_support::http(vec![
            rejected,
            Response::ok(body.as_bytes()),
            Response::ok(body.as_bytes()),
        ])
        .await;
        let backend = Grok {
            client: test_support::client(),
            omit_optional: Arc::new(AtomicBool::new(false)),
        };
        for _ in 0..2 {
            let (tx, _rx) = mpsc::channel(32);
            backend
                .stream(request(url.clone()), tx, CancellationToken::new())
                .await
                .unwrap();
        }
        let captured = server.await.unwrap();
        assert_eq!(captured.len(), 3);
        assert_eq!(captured[0].body["reasoning_effort"], "low");
        assert!(captured[1].body.get("reasoning_effort").is_none());
        assert!(captured[2].body.get("reasoning_effort").is_none());
    }
    #[test]
    fn fragmented_utf8_and_comments() {
        let input = ": ping\r\ndata: {\"text\":\"¿sí?\"}\r\n\r\ndata: [DONE]\n\n".as_bytes();
        for size in 1..input.len() {
            let mut p = SseDecoder::default();
            let mut out = Vec::new();
            for block in input.chunks(size) {
                out.extend(p.push(block).unwrap());
            }
            assert_eq!(out, vec!["{\"text\":\"¿sí?\"}", "[DONE]"]);
        }
    }
    #[test]
    fn multiline_event_and_eof() {
        let mut p = SseDecoder::default();
        assert!(p
            .push(b"event: message\ndata: one\ndata:two")
            .unwrap()
            .is_empty());
        assert_eq!(p.finish().unwrap(), vec!["one\ntwo"]);
    }
    #[test]
    fn bare_cr() {
        assert_eq!(
            SseDecoder::default()
                .push(b"data: a\r\rdata: b\r\r")
                .unwrap(),
            vec!["a", "b"]
        );
    }
    #[test]
    fn invalid_utf8_fails() {
        assert!(SseDecoder::default().push(b"data: \xff\n").is_err());
    }
    #[test]
    fn tool_arguments_fragmented_and_interleaved() {
        let mut a = ToolAssembler::default();
        a.push(&json!([{"index":0,"id":"a","function":{"name":"run_coding_agent","arguments":"{\"ta"}},{"index":1,"id":"b","function":{"name":"coding_agent_status","arguments":"{"}}])).unwrap();
        a.push(&json!([{"index":1,"function":{"arguments":"}"}},{"index":0,"function":{"arguments":"sk\":\"hola\"}"}}])).unwrap();
        let calls = a.finish().unwrap();
        assert_eq!(calls[0].function.arguments, "{\"task\":\"hola\"}");
        assert_eq!(calls[1].function.arguments, "{}");
    }
    #[test]
    fn unfinished_tool_rejected() {
        let mut a = ToolAssembler::default();
        a.push(&json!([{"index":0,"id":"a","function":{"name":"tool","arguments":"{"}}]))
            .unwrap();
        assert!(a.finish().is_err());
    }
    #[tokio::test]
    async fn mock_is_cancellable() {
        let (tx, _rx) = mpsc::channel(16);
        let token = CancellationToken::new();
        token.cancel();
        let request = LlmRequest {
            config: crate::config::Config::parse("").unwrap().llm,
            key: zeroize::Zeroizing::new(String::new()),
            messages: vec![],
            allow_tools: true,
        };
        assert!(matches!(
            MockLlm.stream(request, tx, token).await,
            Err(LlmError::Cancelled)
        ));
    }
}
