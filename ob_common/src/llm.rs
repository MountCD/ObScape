use crate::config::{self, Config};
use crate::database::{
    ContentStruc, Database, JsonMessageContent, JsonRequestMessage, Roles,
};
use reqwest::Client;
use serde::Serialize;
use serde_json::{Value, json};
//use std::time::SystemTime;

#[derive(Serialize, Debug, Clone, Copy)]
pub enum Models {
    TalkModel,
    WorkerModel,
    AudioModel,
}

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
    model: Models,
    history: &mut Vec<JsonMessageContent>,
    message: String,
) -> Result<String, LlmError> {
    let (model_id, api_url, api_key) = match model {
        Models::TalkModel => (&cfg.talk.model_id, &cfg.talk.api_url, &cfg.talk.api_key),
        Models::WorkerModel => (
            &cfg.worker.model_id,
            &cfg.worker.api_url,
            &cfg.worker.api_key,
        ),
        Models::AudioModel => (&cfg.audio.model_id, &cfg.audio.api_url, &cfg.audio.api_key),
    };

    if cfg.verbose {
        println!("[verbose] Requesting LLM: {} to {}", model_id, api_url);
    }

    // Дополним историю пользовательским сообщением.
    let now = chrono_like_now();
    history.push(JsonMessageContent::new(
        Roles::User,
        ContentStruc::new(now, 0, 0, message),
    ));

    let json_message = JsonRequestMessage::new(model_id.clone(), history.clone(), false);
    let req = json!(json_message);

    let mut req = http.post(api_url).json(&req);
    req = req.bearer_auth(api_key);
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
pub async fn make_request(client: &Client, model: Models, message: String) -> anyhow::Result<String> {
    let cfg = config::load_config();
    let db = Database::open_db(&cfg.database_url).await?;
    let mut history = db.export_messages().await?;
    let reply = make_request_with(client, &cfg, model, &mut history, message).await?;
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
