use crate::config::{AgentConfig, Backend, Config};
use crate::database::{JsonMessageContent, JsonRequestMessage, LlmMessage, Roles, ToolCall};
use futures_util::StreamExt;
use reqwest::Client;
use std::pin::Pin;
use tokio_stream::Stream;

/// Ошибки LLM-слоя. Наружу уходят как `ObScapeError::Llm` → HTTP 502.
#[derive(Debug)]
pub enum AgentError {
    /// Сетевая/HTTP-ошибка при обращении к апстриму.
    Http(reqwest::Error),
    /// Ошибка сериализации/десериализации JSON.
    Json(serde_json::Error),
    /// Апстрим вернул ответ без ожидаемого `choices[0].message.content`.
    EmptyResponse,
    /// Апстрим ответил не-2xx. Тело ответа сохраняется: именно там
    /// лежит настоящая причина (`model not found`, `context overflow`).
    Upstream { status: u16, body: String },
    /// Прочие ошибки .
    Other(String),
    /// Не найден агент с таким именем .
    NotFound(String),
    /// Невозможность использования агента, так как он выключен .
    Disabled(String),
}

impl std::fmt::Display for AgentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentError::Http(e) => write!(f, "http: {e}"),
            AgentError::Json(e) => write!(f, "json: {e}"),
            AgentError::EmptyResponse => write!(f, "Empty response from upstream"),
            AgentError::Upstream { status, body } => {
                write!(f, "upstream returned {status}")?;
                let body = body.trim();
                if !body.is_empty() {
                    // Тело режем: в ответе может быть большой кусок чужого
                    // промпта, а наружу уходит сообщение об ошибке.
                    let short: String = body.chars().take(400).collect();
                    write!(f, ": {short}")?;
                }
                Ok(())
            }
            AgentError::Other(s) => write!(f, "{s}"),
            AgentError::NotFound(s) => write!(f, "No agents with that name: {s}"),
            AgentError::Disabled(s) => write!(f, "Agent '{s}' is disabled."),
        }
    }
}

impl std::error::Error for AgentError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            AgentError::Http(e) => Some(e),
            AgentError::Json(e) => Some(e),
            _ => None,
        }
    }
}

impl From<reqwest::Error> for AgentError {
    fn from(e: reqwest::Error) -> Self {
        AgentError::Http(e)
    }
}
impl From<serde_json::Error> for AgentError {
    fn from(e: serde_json::Error) -> Self {
        AgentError::Json(e)
    }
}

pub enum LlmReply {
    Text(String),
    ToolCalls(Vec<ToolCall>),
}

/// Поток дельт ответа модели: каждая `Ok` — очередной кусок текста,
/// `Err` — обрыв (сеть, таймаут, ошибка апстрима).
pub type DeltaStream = Pin<Box<dyn Stream<Item = Result<String, AgentError>> + Send>>;

/// Запрос к модели без стрима: возвращает готовый текст одним куском.
///
/// Общий API для сервера: вызвать LLM, передав уже открытые конфиг,
/// http-клиент и историю диалога. БД и конфиг-файл не читаются.
pub async fn make_request_with(
    http: &Client,
    cfg: &Config,
    agent: &AgentConfig,
    history: &[JsonMessageContent],
) -> Result<String, AgentError> {
    // История передаётся как есть: сообщение пользователя должно быть
    // уже добавлено в неё вызывающей стороной.
    let wire_history: Vec<LlmMessage> = history.iter().map(LlmMessage::from).collect();
    match agent.backend {
        Backend::OpenAI => make_openai_request(http, cfg, agent, wire_history).await,
        Backend::Ollama => make_ollama_request(cfg, agent, wire_history).await,
    }
}

/// Запрос к модели со стримом: возвращает поток дельт.
///
/// Ошибки установления соединения и не-2xx приходят первым элементом
/// потока, чтобы вызывающий код (SSE-хэндлер) обрабатывал их единообразно.
pub async fn make_stream_request(
    http: &Client,
    cfg: &Config,
    agent: &AgentConfig,
    history: &[JsonMessageContent],
) -> Result<DeltaStream, AgentError> {
    let wire_history: Vec<LlmMessage> = history.iter().map(LlmMessage::from).collect();
    match agent.backend {
        Backend::OpenAI => stream_openai(http, cfg, agent, wire_history).await,
        Backend::Ollama => stream_ollama(cfg, agent, wire_history).await,
    }
}

/// Тело OpenAI-совместимого запроса — общее для стрима и не-стрима.
fn openai_body(
    agent: &AgentConfig,
    wire_history: Vec<LlmMessage>,
    stream: bool,
) -> serde_json::Value {
    serde_json::json!(JsonRequestMessage::new(
        agent.model_id.clone(),
        wire_history,
        stream
    ))
}

/// Собрать `reqwest::RequestBuilder` с авторизацией и per-agent таймаутом.
fn openai_request(
    http: &Client,
    agent: &AgentConfig,
    body: &serde_json::Value,
) -> reqwest::RequestBuilder {
    http.post(agent.api_url.clone())
        .bearer_auth(agent.api_key.clone())
        .json(body)
        .timeout(std::time::Duration::from_secs(agent.timeout_secs))
}

/// OpenAI-совместимый chat completions без стрима (прежний путь).
async fn make_openai_request(
    http: &Client,
    cfg: &Config,
    agent: &AgentConfig,
    wire_history: Vec<LlmMessage>,
) -> Result<String, AgentError> {
    let body = openai_body(agent, wire_history, false);
    let response = openai_request(http, agent, &body).send().await?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        crate::vlog!(cfg, "upstream {status}, body: {body}");
        return Err(AgentError::Upstream {
            status: status.as_u16(),
            body,
        });
    }
    let response = response.json::<serde_json::Value>().await?;

    let content = response["choices"][0]["message"]["content"]
        .as_str()
        .ok_or(AgentError::EmptyResponse)?
        .to_string();

    Ok(content)
}

/// OpenAI-совместимый стрим: SSE-чанки `data: {...}` до `data: [DONE]`.
///
/// Каждый чанк отдаётся вызывающему сразу после получения, поэтому
/// ответ модели виден по мере генерации, а соединение не простаивает
/// в ожидании всего ответа.
async fn stream_openai(
    http: &Client,
    cfg: &Config,
    agent: &AgentConfig,
    wire_history: Vec<LlmMessage>,
) -> Result<DeltaStream, AgentError> {
    let body = openai_body(agent, wire_history, true);
    crate::vlog!(
        cfg,
        "openai stream request to {} model `{}`",
        agent.api_url,
        agent.model_id
    );
    let response = openai_request(http, agent, &body).send().await?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        crate::vlog!(cfg, "upstream {status}, body: {body}");
        return Err(AgentError::Upstream {
            status: status.as_u16(),
            body,
        });
    }
    Ok(Box::pin(sse_deltas(response)))
}

/// Разбор SSE-потока в поток текстовых дельт.
///
/// Строки разбираются вручную (без стороннего SSE-парсера): нас
/// интересуют только строки `data:`. Буфер нужен потому, что сетевой
/// чанк может оборвать строку посередине.
fn sse_deltas(response: reqwest::Response) -> impl Stream<Item = Result<String, AgentError>> {
    async_stream::stream! {
        let mut buf = String::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = match chunk {
                Ok(c) => c,
                Err(e) => {
                    yield Err(AgentError::Http(e));
                    return;
                }
            };
            buf.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(pos) = buf.find('\n') {
                let line: String = buf.drain(..=pos).collect();
                match parse_sse_line(line.trim_end_matches(['\r', '\n'])) {
                    SseLine::Delta(text) => yield Ok(text),
                    SseLine::Done => return,
                    SseLine::None => {}
                }
            }
        }
        // Апстрим закрыл соединение без `[DONE]`: данные уже отданы,
        // а вызывающий сохранит то, что успел получить.
    }
}

/// Одна значимая строка SSE-потока.
enum SseLine {
    /// Очередная порция текста.
    Delta(String),
    /// `data: [DONE]` — конец ответа.
    Done,
    /// Служебная строка (пустая, `event:`, комментарий) или чанк без текста.
    None,
}

fn parse_sse_line(line: &str) -> SseLine {
    let Some(data) = line.strip_prefix("data:") else {
        return SseLine::None;
    };
    let data = data.trim();
    if data == "[DONE]" {
        return SseLine::Done;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(data) else {
        return SseLine::None;
    };
    // Основной путь — `delta.content`; `message.content` встречается
    // у серверов, которые отдают один чанк вместо потока.
    let delta = value["choices"][0]["delta"]["content"]
        .as_str()
        .or_else(|| value["choices"][0]["message"]["content"].as_str())
        .unwrap_or_default();
    if delta.is_empty() {
        SseLine::None
    } else {
        SseLine::Delta(delta.to_string())
    }
}

/// Локальный Ollama через `ollama-rs`. `agent.api_url` — адрес сервера
/// (`http://127.0.0.1:11434`), `agent.model_id` — имя модели.
async fn make_ollama_request(
    cfg: &Config,
    agent: &AgentConfig,
    wire_history: Vec<LlmMessage>,
) -> Result<String, AgentError> {
    use ollama_rs::generation::chat::request::ChatMessageRequest;

    let ollama = build_ollama(agent)?;
    let messages = ollama_messages(wire_history)?;
    let request = ChatMessageRequest::new(agent.model_id.clone(), messages);
    crate::vlog!(
        cfg,
        "ollama request to {} model `{}`",
        agent.api_url,
        agent.model_id
    );
    // У ollama-rs своего таймаута нет — свой задаём явно.
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(agent.timeout_secs),
        ollama.send_chat_messages(request),
    )
    .await
    .map_err(|_| {
        AgentError::Other(format!(
            "ollama request timed out after {}s",
            agent.timeout_secs
        ))
    })?
    .map_err(|e| {
        crate::vlog!(cfg, "ollama error: {e}");
        AgentError::Other(format!("ollama: {e}"))
    })?;

    let content = response.message.content;
    if content.is_empty() {
        return Err(AgentError::EmptyResponse);
    }
    Ok(content)
}

/// Стрим Ollama через `ollama-rs` (`send_chat_messages_stream`).
///
/// У `ChatMessageResponseStream` нет таймаута на весь поток: держим
/// ограничение на установление соединения и на паузу между чанками.
async fn stream_ollama(
    cfg: &Config,
    agent: &AgentConfig,
    wire_history: Vec<LlmMessage>,
) -> Result<DeltaStream, AgentError> {
    use ollama_rs::generation::chat::request::ChatMessageRequest;

    let ollama = build_ollama(agent)?;
    let messages = ollama_messages(wire_history)?;
    let request = ChatMessageRequest::new(agent.model_id.clone(), messages);
    crate::vlog!(
        cfg,
        "ollama stream request to {} model `{}`",
        agent.api_url,
        agent.model_id
    );
    let stream = tokio::time::timeout(
        std::time::Duration::from_secs(agent.timeout_secs),
        ollama.send_chat_messages_stream(request),
    )
    .await
    .map_err(|_| {
        AgentError::Other(format!(
            "ollama stream did not start within {}s",
            agent.timeout_secs
        ))
    })?
    .map_err(|e| {
        crate::vlog!(cfg, "ollama stream error: {e}");
        AgentError::Other(format!("ollama: {e}"))
    })?;

    let cfg = cfg.clone();
    let timeout = std::time::Duration::from_secs(agent.timeout_secs);
    let mut stream = stream;
    let collect = async_stream::stream! {
        loop {
            let next = tokio::time::timeout(timeout, stream.next()).await;
            match next {
                // Поток закончился.
                Ok(None) => return,
                Ok(Some(Err(_))) => {
                    // `ollama-rs` теряет причину и отдаёт `()`.
                    crate::vlog!(&cfg, "ollama stream failed");
                    yield Err(AgentError::Other(
                        "ollama: stream failed (see ollama server logs)".to_string(),
                    ));
                    return;
                }
                Ok(Some(Ok(chunk))) => {
                    if !chunk.message.content.is_empty() {
                        yield Ok(chunk.message.content);
                    }
                    if chunk.done {
                        return;
                    }
                }
                Err(_) => {
                    yield Err(AgentError::Other(format!(
                        "ollama stream stalled for {}s",
                        timeout.as_secs()
                    )));
                    return;
                }
            }
        }
    };
    Ok(Box::pin(collect))
}

/// Общий конструктор клиента Ollama.
fn build_ollama(agent: &AgentConfig) -> Result<ollama_rs::Ollama, AgentError> {
    ollama_rs::Ollama::try_new(agent.api_url.clone())
        .map_err(|e| AgentError::Other(format!("invalid ollama api_url `{}`: {e}", agent.api_url)))
}

/// История чата → сообщения Ollama. Картинки (`save_media` не влияет)
/// уходят как `Image::from_base64`, текст — как есть.
fn ollama_messages(
    wire_history: Vec<LlmMessage>,
) -> Result<Vec<ollama_rs::generation::chat::ChatMessage>, AgentError> {
    use ollama_rs::generation::chat::ChatMessage;
    use ollama_rs::generation::images::Image;

    wire_history
        .into_iter()
        .map(|m| {
            let text = m
                .content
                .as_ref()
                .and_then(|c| c.as_text())
                .unwrap_or_default()
                .to_string();
            let images: Vec<Image> = m
                .content
                .as_ref()
                .map(|c| {
                    c.data_urls()
                        .iter()
                        .filter_map(|url| base64_payload(url))
                        .map(Image::from_base64)
                        .collect()
                })
                .unwrap_or_default();
            Ok(match (m.role, images.is_empty()) {
                (Roles::System, _) => ChatMessage::system(text),
                (Roles::User, false) => ChatMessage::user(text).with_images(images),
                (Roles::User, true) => ChatMessage::user(text),
                (Roles::Assistant, _) => ChatMessage::assistant(text),
                (Roles::Tool, _) => ChatMessage::tool(text),
            })
        })
        .collect()
}

/// Вытащить base64 из data-URL (`data:image/png;base64,<data>`).
fn base64_payload(url: &str) -> Option<String> {
    let (_, data) = url.split_once(",")?;
    Some(data.to_string())
}

/// Найти агента по ключу из `config.agents` и убедиться, что он включён.
pub fn resolve_agent(cfg: &Config, kind: &str) -> Result<AgentConfig, AgentError> {
    let agent = cfg
        .agents
        .get(kind)
        .ok_or_else(|| AgentError::NotFound(kind.to_string()))?;
    if !agent.enabled {
        return Err(AgentError::Disabled(kind.to_string()));
    }
    Ok(agent.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_openai_delta_chunks() {
        let chunk = r#"data: {"choices":[{"delta":{"content":"Hel"}}]}"#;
        assert!(matches!(parse_sse_line(chunk), SseLine::Delta(t) if t == "Hel"));

        // Пустая дельта (роль, первый чанк) — не текст.
        let role_only = r#"data: {"choices":[{"delta":{"role":"assistant"}}]}"#;
        assert!(matches!(parse_sse_line(role_only), SseLine::None));

        // Некоторые серверы отдают один чанк с `message`.
        let full = r#"data: {"choices":[{"message":{"content":"ok"}}]}"#;
        assert!(matches!(parse_sse_line(full), SseLine::Delta(t) if t == "ok"));
    }

    #[test]
    fn parses_done_and_ignores_service_lines() {
        assert!(matches!(parse_sse_line("data: [DONE]"), SseLine::Done));
        assert!(matches!(parse_sse_line("data:[DONE]"), SseLine::Done));
        assert!(matches!(parse_sse_line(""), SseLine::None));
        assert!(matches!(parse_sse_line("event: message"), SseLine::None));
        assert!(matches!(parse_sse_line(": keep-alive"), SseLine::None));
        assert!(matches!(parse_sse_line("data: not-json"), SseLine::None));
    }

    #[test]
    fn upstream_error_display_includes_body() {
        let err = AgentError::Upstream {
            status: 400,
            body: r#"{"error":"model not found"}"#.into(),
        };
        let text = err.to_string();
        assert!(text.contains("upstream returned 400"), "{text}");
        assert!(text.contains("model not found"), "{text}");
    }

    #[test]
    fn upstream_error_body_is_truncated() {
        let err = AgentError::Upstream {
            status: 500,
            body: "я".repeat(1000),
        };
        let text = err.to_string();
        assert_eq!(text.chars().filter(|c| *c == 'я').count(), 400);
    }

    #[test]
    fn base64_payload_extracts_data_url_body() {
        assert_eq!(
            base64_payload("data:image/png;base64,AAAA").as_deref(),
            Some("AAAA")
        );
        assert_eq!(base64_payload("not-a-data-url"), None);
    }
}
