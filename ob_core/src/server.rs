use crate::Assistant;
use crate::config::Config;
use crate::database::Database;
use axum::{
    Json, Router,
    extract::{Request, State},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
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
/// Сколько ждём `SELECT 1` в `/v1/health`.
const HEALTH_DB_TIMEOUT: Duration = Duration::from_secs(2);

/// Состояние, разделяемое между всеми хэндлерами.
#[derive(Clone)]
pub struct AppState {
    pub assistant: Arc<Assistant>,
    pub db: Database,
    /// Bearer-токен для `/v1/chat/*`; `None` — аутентификация выключена.
    pub api_token: Option<Arc<str>>,
}

impl AppState {
    pub fn new(db: Database, cfg: Config) -> Self {
        AppState {
            api_token: cfg.api_token.as_deref().map(Arc::from),
            db: db.clone(),
            assistant: Arc::new(Assistant::new(db, cfg)),
        }
    }
}

/// Корневой роутер. `/v1/health` открыт всегда, `/v1/chat/*` — за токеном,
/// если он задан.
pub fn router(state: AppState) -> Router {
    let chat = Router::new()
        .route("/v1/chat/new", post(create_chat))
        .route("/v1/chat/message", post(post_message))
        .route_layer(middleware::from_fn_with_state(state.clone(), require_token));
    Router::new()
        .route("/v1/health", get(health))
        .merge(chat)
        .layer(RequestBodyLimitLayer::new(MAX_BODY_BYTES))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            REQUEST_TIMEOUT,
        ))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Проверка `Authorization: Bearer <token>`. Сравнение за постоянное
/// время, чтобы по таймингу нельзя было подбирать токен побайтно.
async fn require_token(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let Some(expected) = state.api_token.as_deref() else {
        return next.run(req).await;
    };
    let presented = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim);
    match presented {
        Some(t) if constant_time_eq(t.as_bytes(), expected.as_bytes()) => next.run(req).await,
        _ => (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Bearer")],
            Json(serde_json::json!({ "error": "missing or invalid bearer token" })),
        )
            .into_response(),
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
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
}

impl AppError {
    /// HTTP-статус для варианта ошибки; текст берётся из `Display`.
    fn status(&self) -> StatusCode {
        match self {
            AppError::BadRequest(_) => StatusCode::BAD_REQUEST,
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
            AppError::BadRequest(m) => write!(f, "{m}"),
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

/// `GET /v1/health` — 200, если БД отвечает на `SELECT 1`, иначе 503.
/// Текст ошибки БД наружу не отдаём.
async fn health(State(state): State<AppState>) -> impl IntoResponse {
    match tokio::time::timeout(HEALTH_DB_TIMEOUT, state.db.ping()).await {
        Ok(Ok(())) => (
            StatusCode::OK,
            Json(serde_json::json!({ "status": "ok", "db": "ok" })),
        ),
        Ok(Err(e)) => {
            tracing::error!("health: database check failed: {e}");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "status": "degraded", "db": "unavailable" })),
            )
        }
        Err(_) => {
            tracing::error!("health: database check timed out");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "status": "degraded", "db": "timeout" })),
            )
        }
    }
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
            api_token: None,
            agents,
        }
    }

    fn app(api_url: &str) -> Router {
        app_with(test_config(api_url))
    }

    fn app_with(cfg: Config) -> Router {
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
    async fn health_reports_unreachable_db() {
        // Ленивый пул на закрытый порт: SELECT 1 не проходит → 503.
        let req = Request::get("/v1/health").body(Body::empty()).unwrap();
        let (status, json) = call(app("http://127.0.0.1:1/"), req).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(json["status"], "degraded");
    }

    #[tokio::test]
    async fn chat_routes_require_token_when_configured() {
        let mut cfg = test_config("http://127.0.0.1:1/");
        cfg.api_token = Some("s3cret".into());
        let body = r#"{"user_id":1,"message":"hi"}"#;

        // Без токена и с неверным — 401, до валидации тела.
        let (status, json) = call(
            app_with(cfg.clone()),
            post_json("/v1/chat/new", body.into()),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(json["error"].as_str().unwrap().contains("bearer"));

        let req = Request::post("/v1/chat/new")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::AUTHORIZATION, "Bearer wrong")
            .body(Body::from(body))
            .unwrap();
        let (status, _) = call(app_with(cfg.clone()), req).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        // С верным токеном проходим до хэндлера (пустое сообщение → 400).
        let req = Request::post("/v1/chat/new")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::AUTHORIZATION, "Bearer s3cret")
            .body(Body::from(r#"{"user_id":1,"message":" "}"#))
            .unwrap();
        let (status, json) = call(app_with(cfg.clone()), req).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(json["error"], "message is empty");

        // /v1/health токена не требует.
        let req = Request::get("/v1/health").body(Body::empty()).unwrap();
        let (status, _) = call(app_with(cfg), req).await;
        assert_ne!(status, StatusCode::UNAUTHORIZED);
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

        // Мок апстрима: отдаёт "pong" и запоминает последний запрос;
        // на реплику "fail" отвечает 500.
        let seen: Arc<std::sync::Mutex<Option<serde_json::Value>>> = Arc::default();
        let seen_in_mock = seen.clone();
        let mock = Router::new().route(
            "/v1/",
            post(move |Json(body): Json<serde_json::Value>| {
                let seen = seen_in_mock.clone();
                async move {
                    let last = body["messages"]
                        .as_array()
                        .and_then(|m| m.last())
                        .and_then(|m| m["content"].as_str())
                        .unwrap_or_default()
                        .to_string();
                    *seen.lock().unwrap() = Some(body);
                    if last == "fail" {
                        return (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            Json(serde_json::json!({})),
                        );
                    }
                    (
                        StatusCode::OK,
                        Json(serde_json::json!({
                            "choices": [{ "message": { "content": "pong" } }]
                        })),
                    )
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
        let (status, json) = call(app.clone(), post_json("/v1/chat/message", body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(json["error"].as_str().unwrap().contains("unknown chat_id"));

        // Апстрим упал → 502, и реплика пользователя откатывается:
        // следующий запрос не содержит "fail" и двух `user` подряд.
        let body = format!(r#"{{"user_id":42,"chat_id":{chat_id},"message":"fail"}}"#);
        let (status, json) = call(app.clone(), post_json("/v1/chat/message", body)).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY, "{json}");
        assert!(
            json["error"]
                .as_str()
                .unwrap()
                .contains("upstream returned 500")
        );

        let body = format!(r#"{{"user_id":42,"chat_id":{chat_id},"message":"third"}}"#);
        let (status, _) = call(app, post_json("/v1/chat/message", body)).await;
        assert_eq!(status, StatusCode::OK);
        let last = seen.lock().unwrap().clone().unwrap();
        let contents: Vec<&str> = last["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["content"].as_str().unwrap())
            .collect();
        assert!(!contents.contains(&"fail"), "{contents:?}");
        let roles: Vec<&str> = last["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["role"].as_str().unwrap())
            .collect();
        assert_eq!(
            roles,
            ["system", "user", "assistant", "user", "assistant", "user"]
        );
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
