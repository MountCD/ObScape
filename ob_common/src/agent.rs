use crate::config::{self, AgentConfig, Config};
use crate::database::{
    ContentStruc, Database, JsonMessageContent, JsonRequestMessage, LlmMessage, Roles,
};
use reqwest::Client;
use serde_json::{Value, json};

/// Ошибки LLM-слоя. Используется как `AppError::Agent` в HTTP-ответах.
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
    history: &mut Vec<JsonMessageContent>,
    message: String,
) -> Result<String, AgentError> {
    crate::vlog!(
        cfg,
        "requesting LLM: {} at {}",
        agent.model_id,
        agent.api_url
    );

    // Дополним историю пользовательским сообщением.
    let now = crate::time::now_iso();
    history.push(JsonMessageContent::new(
        Roles::User,
        ContentStruc::new(now, 0, 0, message),
    ));

    let wire_history: Vec<LlmMessage> = history.iter().map(LlmMessage::from).collect();
    let json_message = JsonRequestMessage::new(agent.model_id.clone(), wire_history, false);
    let req = json!(json_message);

    let mut req = http.post(agent.api_url.clone()).json(&req);
    req = req.bearer_auth(agent.api_key.clone());
    let response = req.send().await?;
    let response = response.json::<Value>().await?;

    let content = response["choices"][0]["message"]["content"]
        .as_str()
        .ok_or(AgentError::EmptyResponse)?
        .to_string();

    crate::vlog!(cfg, "LLM response received: {} chars", content.len());

    Ok(content)
}

/// Старый API, оставлен ради существующих вызовов (например, тестов).
/// Открывает конфиг и БД самостоятельно — удобно для одноразовых
/// CLI-вызовов, но в HTTP-сервере лучше использовать `make_request_with`.
pub async fn make_request(
    client: &Client,
    message: String,
    kind: String,
) -> anyhow::Result<String> {
    let cfg = config::load_config()?;
    let agent = resolve_agent(&cfg, &kind)?;
    let db = Database::open_db(&cfg.database_url).await?;
    let mut history = db.export_messages().await?;
    let reply = make_request_with(client, &cfg, &agent, &mut history, message).await?;
    Ok(reply)
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
