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
use ob_common::time::unix_secs;
use serde::{Deserialize, Serialize};
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
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            REQUEST_TIMEOUT,
        ))
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

impl AppError {
    /// HTTP-статус для варианта ошибки; текст берётся из `Display`.
    fn status(&self) -> StatusCode {
        match self {
            AppError::BadRequest(_) => StatusCode::BAD_REQUEST,
            AppError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
            AppError::Core(e) => match e {
                crate::ObScapeError::BadRequest(_) => StatusCode::BAD_REQUEST,
                crate::ObScapeError::Llm(_) => StatusCode::BAD_GATEWAY,
                crate::ObScapeError::Db(_) | crate::ObScapeError::Internal(_) => {
                    StatusCode::INTERNAL_SERVER_ERROR
                }
            },
        }
    }
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AppError::BadRequest(m) | AppError::Internal(m) => write!(f, "{m}"),
            AppError::Core(e) => write!(f, "{e}"),
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> axum::response::Response {
        let body = Json(serde_json::json!({ "error": self.to_string() }));
        (self.status(), body).into_response()
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, header};
    use http_body_util::BodyExt;
    use ob_common::config::AgentConfig;
    use std::collections::HashMap;
    use tower::ServiceExt;

    /// Конфиг с одним агентом, смотрящим на `api_url`.
    fn test_config(api_url: &str) -> Config {
        let mut agents = HashMap::new();
        agents.insert(
            "primary".to_string(),
            AgentConfig {
                enabled: true,
                main: true,
                model_id: "test-model".into(),
                api_url: api_url.into(),
                api_key: "k".into(),
                personality_prompt: "p".into(),
                timeout_secs: 5,
            },
        );
        Config {
            autogenerated: false,
            // Пул ленивый: до БД тесты не доходят.
            database_url: "postgres://u:p@127.0.0.1:1/db".into(),
            http_bind: None,
            verbose: false,
            shared_prompt: "s".into(),
            history_limit: 50,
            agents,
        }
    }

    fn app(api_url: &str) -> Router {
        let cfg = test_config(api_url);
        let db = Database::connect_lazy(&cfg.database_url).expect("lazy pool");
        router(AppState::new(db, cfg))
    }

    async fn call(app: Router, req: Request<Body>) -> (StatusCode, serde_json::Value) {
        let resp = app.oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, json)
    }

    fn post_json(uri: &str, body: String) -> Request<Body> {
        Request::post(uri)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body))
            .unwrap()
    }

    #[tokio::test]
    async fn health_is_ok() {
        let req = Request::get("/v1/health").body(Body::empty()).unwrap();
        let (status, json) = call(app("http://127.0.0.1:1/"), req).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["status"], "ok");
    }

    #[tokio::test]
    async fn empty_message_is_bad_request() {
        for uri in ["/v1/chat/new", "/v1/chat/message"] {
            let body = r#"{"user_id":1,"chat_id":1,"message":"   ","agent":"primary"}"#;
            let (status, json) =
                call(app("http://127.0.0.1:1/"), post_json(uri, body.into())).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
            assert_eq!(json["error"], "message is empty");
        }
    }

    #[tokio::test]
    async fn too_long_message_is_bad_request() {
        let msg = "a".repeat(MAX_MESSAGE_CHARS + 1);
        let body = serde_json::json!({"user_id": 1, "message": msg, "agent": "primary"});
        let (status, json) = call(
            app("http://127.0.0.1:1/"),
            post_json("/v1/chat/new", body.to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(json["error"].as_str().unwrap().contains("too long"));
    }

    #[tokio::test]
    async fn oversized_body_is_rejected() {
        let body = "x".repeat(MAX_BODY_BYTES + 1);
        let (status, _) = call(app("http://127.0.0.1:1/"), post_json("/v1/chat/new", body)).await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn non_positive_chat_id_is_bad_request() {
        let body = r#"{"user_id":1,"chat_id":0,"message":"hi"}"#;
        let (status, json) = call(
            app("http://127.0.0.1:1/"),
            post_json("/v1/chat/message", body.into()),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(json["error"], "chat_id must be positive");
    }

    /// Сквозной тест: реальный Postgres (из `OBSCAPE_TEST_DATABASE_URL`)
    /// и замоканный OpenAI-совместимый апстрим на локальном порту.
    /// Без переменной окружения тест пропускается.
    #[tokio::test]
    async fn chat_round_trip_with_mocked_upstream() {
        let Ok(db_url) = std::env::var("OBSCAPE_TEST_DATABASE_URL") else {
            eprintln!("OBSCAPE_TEST_DATABASE_URL is not set, skipping");
            return;
        };

        // Мок апстрима: отдаёт "pong" и запоминает последний запрос.
        let seen: Arc<std::sync::Mutex<Option<serde_json::Value>>> = Arc::default();
        let seen_in_mock = seen.clone();
        let mock = Router::new().route(
            "/v1/",
            post(move |Json(body): Json<serde_json::Value>| {
                let seen = seen_in_mock.clone();
                async move {
                    *seen.lock().unwrap() = Some(body);
                    Json(serde_json::json!({
                        "choices": [{ "message": { "content": "pong" } }]
                    }))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });

        let cfg = test_config(&format!("http://{addr}/v1/"));
        let db = Database::open_db(&db_url).await.expect("postgres");
        let app = router(AppState::new(db, cfg));

        // Новый чат без `agent` → main-агент.
        let body = r#"{"user_id":42,"message":"ping"}"#;
        let (status, json) = call(app.clone(), post_json("/v1/chat/new", body.into())).await;
        assert_eq!(status, StatusCode::OK, "{json}");
        assert_eq!(json["message"], "pong");
        let chat_id = json["chat_id"].as_i64().unwrap();

        // Второе сообщение в тот же чат.
        let body = format!(r#"{{"user_id":42,"chat_id":{chat_id},"message":"again"}}"#);
        let (status, json) = call(app.clone(), post_json("/v1/chat/message", body)).await;
        assert_eq!(status, StatusCode::OK, "{json}");
        assert_eq!(json["message"], "pong");

        // Апстрим получил: system, user, assistant, user — без дублей.
        let last = seen.lock().unwrap().clone().unwrap();
        let roles: Vec<&str> = last["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["role"].as_str().unwrap())
            .collect();
        assert_eq!(roles, ["system", "user", "assistant", "user"]);
        assert_eq!(last["messages"][3]["content"], "again");

        // Чужой user_id не видит чат.
        let body = format!(r#"{{"user_id":7,"chat_id":{chat_id},"message":"steal"}}"#);
        let (status, json) = call(app, post_json("/v1/chat/message", body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(json["error"].as_str().unwrap().contains("unknown chat_id"));
    }

    #[tokio::test]
    async fn unknown_agent_is_bad_gateway_before_touching_db() {
        let body = r#"{"user_id":1,"message":"hi","agent":"nope"}"#;
        let (status, json) = call(
            app("http://127.0.0.1:1/"),
            post_json("/v1/chat/new", body.into()),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert!(json["error"].as_str().unwrap().contains("nope"));
    }
}
