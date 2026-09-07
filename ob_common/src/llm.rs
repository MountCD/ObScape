use crate::config::{self, AgentConfig, Config};
use crate::database::{ContentStruc, Database, JsonMessageContent, JsonRequestMessage, Roles};
use reqwest::Client;
//use serde::Serialize;
use serde_json::{Value, json};
//use std::time::SystemTime;

/// Ошибки LLM-слоя. Используется как `AppError::Llm` в HTTP-ответах.
#[derive(Debug)]
pub enum LlmError {
    /// Сетевая/HTTP-ошибка при обращении к апстриму.
    Http(reqwest::Error),
    /// Ошибка сериализации/десериализации JSON.
    Json(serde_json::Error),
    /// Апстрим вернул ответ без ожидаемого `choices[0].message.content`.
    EmptyResponse,
    /// Прочие ошибки (например, неизвестный вариант `Models`).
    Other(String),
}

impl std::fmt::Display for LlmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LlmError::Http(e) => write!(f, "http: {e}"),
            LlmError::Json(e) => write!(f, "json: {e}"),
            LlmError::EmptyResponse => write!(f, "empty response from upstream"),
            LlmError::Other(s) => write!(f, "{s}"),
        }
    }
}

impl std::error::Error for LlmError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            LlmError::Http(e) => Some(e),
            LlmError::Json(e) => Some(e),
            _ => None,
        }
    }
}

impl From<reqwest::Error> for LlmError {
    fn from(e: reqwest::Error) -> Self {
        LlmError::Http(e)
    }
}
impl From<serde_json::Error> for LlmError {
    fn from(e: serde_json::Error) -> Self {
        LlmError::Json(e)
    }
}

/// Публичный API для сервера: вызвать LLM, передав уже открытые
/// конфиг, http-клиент и историю диалога. Не открывает БД и не читает
/// конфиг — всё передаётся снаружи.
pub async fn make_request_with(
    http: &Client,
    cfg: &Config,
    history: &mut Vec<JsonMessageContent>,
    message: String,
    agent: AgentConfig,
) -> Result<String, LlmError> {
    if cfg.verbose {
        println!(
            "[verbose] Requesting LLM: {} to {}",
            agent.model_id, agent.api_url
        );
    }

    // Дополним историю пользовательским сообщением.
    let now = chrono_like_now();
    history.push(JsonMessageContent::new(
        Roles::User,
        ContentStruc::new(now, 0, 0, message),
    ));

    let json_message = JsonRequestMessage::new(agent.model_id.clone(), history.clone(), false);
    let req = json!(json_message);

    let mut req = http.post(agent.api_url).json(&req);
    req = req.bearer_auth(agent.api_key);
    let response = req.send().await?;
    let response = response.json::<Value>().await?;

    let content = response["choices"][0]["message"]["content"]
        .as_str()
        .ok_or(LlmError::EmptyResponse)?
        .to_string();

    if cfg.verbose {
        println!("[verbose] LLM response received: {} chars", content.len());
    }

    Ok(content)
}

/// Старый API, оставлен ради существующих вызовов (например, тестов).
/// Открывает конфиг и БД самостоятельно — удобно для одноразовых
/// CLI-вызовов, но в HTTP-сервере лучше использовать `make_request_with`.
pub async fn make_request(
    client: &Client,
    message: String,
    agent: AgentConfig,
) -> anyhow::Result<String> {
    let cfg = config::load_config();
    let db = Database::open_db(&cfg.database_url).await?;
    let mut history = db.export_messages().await?;
    let reply = make_request_with(client, &cfg, &mut history, message, agent).await?;
    Ok(reply)
}

/// Простейшая метка времени в формате ISO-8601, без подтягивания `chrono`.
/// Достаточно для поля `time` в БД.
fn chrono_like_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("1970-01-01T00:00:{secs}Z")
}
