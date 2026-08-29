// HTTP-сервер ядра: axum 0.7, tokio.
// Принимает сообщения от внешних приложений, гоняет их через LLM,
// сохраняет историю в Postgres и возвращает ответ ассистента.
use crate::config::Config;
use crate::database::{ContentStruc, Database, JsonMessageContent, Roles};
use crate::llm::{self, Models};
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
    pub db: Arc<Database>,
    pub cfg: Arc<Config>,
    pub http: Arc<reqwest::Client>,
}

impl AppState {
    pub fn new(db: Database, cfg: Config) -> Self {
        AppState {
            db: Arc::new(db),
            cfg: Arc::new(cfg),
            http: Arc::new(reqwest::Client::new()),
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
    Db(sqlx::Error),
    Llm(llm::LlmError),
    Internal(String),
}

impl IntoResponse for AppError {
    fn into_response(self) -> axum::response::Response {
        let (status, msg) = match &self {
            AppError::BadRequest(m) => (StatusCode::BAD_REQUEST, m.clone()),
            AppError::Db(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("db error: {e}"),
            ),
            AppError::Llm(e) => (
                StatusCode::BAD_GATEWAY,
                format!("upstream llm error: {e}"),
            ),
            AppError::Internal(m) => (StatusCode::INTERNAL_SERVER_ERROR, m.clone()),
        };
        (status, Json(serde_json::json!({ "error": msg }))).into_response()
    }
}

impl From<sqlx::Error> for AppError {
    fn from(e: sqlx::Error) -> Self {
        AppError::Db(e)
    }
}
impl From<llm::LlmError> for AppError {
    fn from(e: llm::LlmError) -> Self {
        AppError::Llm(e)
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

    // 1. Выделить chat_id = MAX(chat_id) + 1.
    let chat_id = next_chat_id(&state.db).await?;
    let now = unix_secs();

    // 2. Сохранить пользовательское сообщение.
    state
        .db
        .add_message(JsonMessageContent::new(
            Roles::User,
            ContentStruc::new(iso_from_unix(now), chat_id, req.user_id, req.message.clone()),
        ))
        .await?;

    // 3. Собрать историю (только что сохранённое сообщение) и вызвать LLM.
    let mut history = state.db.export_chat(chat_id).await?;
    let reply = llm::make_request_with(
        &state.http,
        &state.cfg,
        pick_model(&state.cfg, chat_id),
        &mut history,
        req.message,
    )
    .await?;

    // 4. Сохранить ответ ассистента.
    let reply_time = unix_secs();
    state
        .db
        .add_message(JsonMessageContent::new(
            Roles::Assistant,
            ContentStruc::new(iso_from_unix(reply_time), chat_id, req.user_id, reply.clone()),
        ))
        .await?;

    Ok(Json(AssistantOut::new(chat_id, reply)))
}

/// `POST /v1/chat/messages` — добавить сообщение в существующий чат.
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

    let now = unix_secs();

    // 1. Сохранить пользовательское сообщение.
    state
        .db
        .add_message(JsonMessageContent::new(
            Roles::User,
            ContentStruc::new(
                iso_from_unix(now),
                req.chat_id,
                req.user_id,
                req.message.clone(),
            ),
        ))
        .await?;

    // 2. Подтянуть историю чата и вызвать LLM.
    let mut history = state.db.export_chat(req.chat_id).await?;
    let reply = llm::make_request_with(
        &state.http,
        &state.cfg,
        pick_model(&state.cfg, req.chat_id),
        &mut history,
        req.message,
    )
    .await?;

    // 3. Сохранить ответ ассистента.
    let reply_time = unix_secs();
    state
        .db
        .add_message(JsonMessageContent::new(
            Roles::Assistant,
            ContentStruc::new(
                iso_from_unix(reply_time),
                req.chat_id,
                req.user_id,
                reply.clone(),
            ),
        ))
        .await?;

    Ok(Json(AssistantOut::new(req.chat_id, reply)))
}

// ---- Вспомогательное ----------------------------------------------------

/// Следующий `chat_id` через `MAX(chat_id) + 1`. Не атомарно — TODO.
async fn next_chat_id(db: &Database) -> Result<i64, AppError> {
    let row: Option<(Option<i64>,)> = sqlx::query_as(r#"SELECT MAX(chat_id) FROM messages"#)
        .fetch_one(db.pool())
        .await?;
    Ok(row.0.unwrap_or(0).saturating_add(1))
}

/// Какую модель использовать для данного чата. Сейчас — TalkModel.
fn pick_model(_cfg: &Config, _chat_id: i64) -> Models {
    // TODO: маппинг chat_id → модель (talk/worker/audio) по правилам,
    // которые позже зададите (например, по диапазону chat_id или по полю в БД).
    Models::TalkModel
}

/// Текущее Unix-время в секундах.
fn unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Грубый ISO-таймстамп из epoch-секунд. Достаточно для поля `time` в БД.
fn iso_from_unix(_secs: i64) -> String {
    // TODO: использовать chrono/time, чтобы получить корректную дату.
    // Сейчас ставим метку, пригодную как уникальный ключ сортировки.
    "1970-01-01T00:00:00Z".to_string()
}
