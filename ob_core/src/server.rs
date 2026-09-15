use crate::Assistant;
use crate::config::Config;
use crate::database::Database;
use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use ob_common::time::unix_secs;
use std::sync::Arc;
use std::time::Duration;
use tower_http::{limit::RequestBodyLimitLayer, timeout::TimeoutLayer, trace::TraceLayer};

/// Максимальный размер тела запроса (JSON с сообщением).
const MAX_BODY_BYTES: usize = 64 * 1024;
/// Максимальная длина `message` в символах.
const MAX_MESSAGE_CHARS: usize = 16 * 1024;
/// Сколько ждём обработку одного запроса (включая апстрим).
const REQUEST_TIMEOUT: Duration = Duration::from_secs(180);

/// Состояние, разделяемое между всеми хэндлерами.
#[derive(Clone)]
pub struct AppState {
    pub assistant: Arc<Assistant>,
    pub cfg: Arc<Config>,
}

impl AppState {
    pub fn new(db: Database, cfg: Config) -> Self {
        AppState {
            assistant: Arc::new(Assistant::new(db, cfg.clone())),
            cfg: Arc::new(cfg),
        }
    }
}

/// Корневой роутер.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/v1/health", get(health))
        .route("/v1/chat/new", post(create_chat))
        .route("/v1/chat/message", post(post_message))
        .layer(RequestBodyLimitLayer::new(MAX_BODY_BYTES))
        .layer(TimeoutLayer::with_status_code(StatusCode::REQUEST_TIMEOUT, REQUEST_TIMEOUT))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Общая валидация текста сообщения для обоих хэндлеров.
fn validate_message(message: &str) -> Result<(), AppError> {
    if message.trim().is_empty() {
        return Err(AppError::BadRequest("message is empty".into()));
    }
    if message.chars().count() > MAX_MESSAGE_CHARS {
        return Err(AppError::BadRequest(format!(
            "message is too long (max {MAX_MESSAGE_CHARS} chars)"
        )));
    }
    Ok(())
}

// ---- DTO ----------------------------------------------------------------

/// Запрос на новое сообщение в существующем чате.
#[derive(Debug, Deserialize)]
pub struct MessageIn {
    pub user_id: i64,
    pub chat_id: i64,
    pub message: String,
}

/// Запрос на создание нового чата с первым сообщением.
#[derive(Debug, Deserialize)]
pub struct NewChatIn {
    pub user_id: i64,
    pub message: String,
    /// Ключ из `config.agents`; если не указан — агент с `main = true`.
    #[serde(default)]
    pub agent: Option<String>,
}

/// Ответ ядра: идентификатор чата, метка времени, текст ассистента, инструменты.
#[derive(Debug, Serialize)]
pub struct AssistantOut {
    pub chat_id: i64,
    /// Unix-time в секундах.
    pub time: i64,
    pub message: String,
    pub tools: Vec<serde_json::Value>,
}

impl AssistantOut {
    fn new(chat_id: i64, message: String) -> Self {
        AssistantOut {
            chat_id,
            time: unix_secs(),
            message,
            tools: Vec::new(),
        }
    }
}

// ---- Ошибки -------------------------------------------------------------

/// Тип ошибки для хэндлеров. `IntoResponse` маппит варианты в HTTP-статусы.
#[derive(Debug)]
pub enum AppError {
    BadRequest(String),
    Core(crate::ObScapeError),
    Internal(String),
}

impl IntoResponse for AppError {
    fn into_response(self) -> axum::response::Response {
        let (status, msg) = match &self {
            AppError::BadRequest(m) => (StatusCode::BAD_REQUEST, m.clone()),
            AppError::Core(e) => match e {
                crate::ObScapeError::Db(err) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("db error: {err}"),
                ),
                crate::ObScapeError::Llm(err) => {
                    (StatusCode::BAD_GATEWAY, format!("llm error: {err}"))
                }
                crate::ObScapeError::BadRequest(m) => (StatusCode::BAD_REQUEST, m.clone()),
                crate::ObScapeError::Internal(m) => (StatusCode::INTERNAL_SERVER_ERROR, m.clone()),
            },
            AppError::Internal(m) => (StatusCode::INTERNAL_SERVER_ERROR, m.clone()),
        };
        (status, Json(serde_json::json!({ "error": msg }))).into_response()
    }
}

// ---- Хэндлеры -----------------------------------------------------------

async fn health() -> impl IntoResponse {
    Json(serde_json::json!({ "status": "ok" }))
}

/// `POST /v1/chat/new` — создать новый чат с первым сообщением пользователя.
async fn create_chat(
    State(state): State<AppState>,
    Json(req): Json<NewChatIn>,
) -> Result<Json<AssistantOut>, AppError> {
    validate_message(&req.message)?;

    let (chat_id, reply) = state
        .assistant
        .create_chat(req.user_id, req.message, req.agent)
        .await
        .map_err(AppError::Core)?;

    Ok(Json(AssistantOut::new(chat_id, reply)))
}

/// `POST /v1/chat/message` — добавить сообщение в существующем чате.
async fn post_message(
    State(state): State<AppState>,
    Json(req): Json<MessageIn>,
) -> Result<Json<AssistantOut>, AppError> {
    validate_message(&req.message)?;
    if req.chat_id <= 0 {
        return Err(AppError::BadRequest("chat_id must be positive".into()));
    }

    let reply = state
        .assistant
        .send_message(req.user_id, req.chat_id, req.message)
        .await
        .map_err(AppError::Core)?;

    Ok(Json(AssistantOut::new(req.chat_id, reply)))
}
