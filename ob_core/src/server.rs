use crate::config::Config;
use crate::database::{ContentStruc, Database, JsonMessageContent, Roles};
use crate::llm::{self, Models};
use crate::{Assistant, ObScapeError};
use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

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
        .route("/v1/chat/messages", post(post_message))
        .with_state(state)
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
    pub ai_type: String,
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
    if req.message.trim().is_empty() {
        return Err(AppError::BadRequest("message is empty".into()));
    }

    let (chat_id, reply) = state
        .assistant
        .create_chat(req.user_id, req.message)
        .await
        .map_err(AppError::Core)?;

    Ok(Json(AssistantOut::new(chat_id, reply)))
}

/// `POST /v1/chat/messages` — добавить сообщение в существующем чате.
async fn post_message(
    State(state): State<AppState>,
    Json(req): Json<MessageIn>,
) -> Result<Json<AssistantOut>, AppError> {
    if req.message.trim().is_empty() {
        return Err(AppError::BadRequest("message is empty".into()));
    }
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

// ---- Вспомогательное ----------------------------------------------------

/// Текущее Unix-время в секундах.
fn unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Грубый ISO-таймстамп из epoch-секунд. Достаточно для поля `time` в БД.
fn iso_from_unix(_secs: i64) -> String {
    "1970-01-01T00:00:00Z".to_string()
}
