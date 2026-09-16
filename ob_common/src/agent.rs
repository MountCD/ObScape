use crate::config::{AgentConfig, Config};
use crate::database::{JsonMessageContent, JsonRequestMessage, LlmMessage};
use reqwest::Client;
use serde_json::{Value, json};

/// Ошибки LLM-слоя. Наружу уходят как `ObScapeError::Llm` → HTTP 502.
#[derive(Debug)]
pub enum AgentError {
    /// Сетевая/HTTP-ошибка при обращении к апстриму.
    Http(reqwest::Error),
    /// Ошибка сериализации/десериализации JSON.
    Json(serde_json::Error),
    /// Апстрим вернул ответ без ожидаемого `choices[0].message.content`.
    EmptyResponse,
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

/// Публичный API для сервера: вызвать LLM, передав уже открытые
/// конфиг, http-клиент и историю диалога. Не открывает БД и не читает
/// конфиг — всё передаётся снаружи.
pub async fn make_request_with(
    http: &Client,
    cfg: &Config,
    agent: &AgentConfig,
    history: &[JsonMessageContent],
) -> Result<String, AgentError> {
    crate::vlog!(
        cfg,
        "requesting LLM: {} at {}",
        agent.model_id,
        agent.api_url
    );

    // История передаётся как есть: сообщение пользователя должно быть
    // уже добавлено в неё вызывающей стороной.
    let wire_history: Vec<LlmMessage> = history.iter().map(LlmMessage::from).collect();
    let json_message = JsonRequestMessage::new(agent.model_id.clone(), wire_history, false);
    let req = json!(json_message);

    let mut req = http.post(agent.api_url.clone()).json(&req);
    req = req
        .bearer_auth(agent.api_key.clone())
        .timeout(std::time::Duration::from_secs(agent.timeout_secs));
    let response = req.send().await?;
    let status = response.status();
    if !status.is_success() {
        // Тело может содержать фрагменты запроса — наружу отдаём только
        // статус, а тело только в verbose-лог.
        let body = response.text().await.unwrap_or_default();
        crate::vlog!(cfg, "upstream returned {status}: {body}");
        return Err(AgentError::Other(format!("upstream returned {status}")));
    }
    let response = response.json::<Value>().await?;

    let content = response["choices"][0]["message"]["content"]
        .as_str()
        .ok_or(AgentError::EmptyResponse)?
        .to_string();

    crate::vlog!(cfg, "LLM response received: {} chars", content.len());

    Ok(content)
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
