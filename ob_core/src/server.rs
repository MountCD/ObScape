use crate::Assistant;
use crate::config::Config;
use crate::database::Database;
use axum::{
    Json, Router,
    extract::{Path, Request, State},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{get, post},
};
use ob_common::time::unix_secs;
use ob_common::vision::{self, VisionInput};
use ob_lib::MediaInput;
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;
use tokio_stream::StreamExt;
use tower_http::{timeout::TimeoutLayer, trace::TraceLayer};

/// Максимальный размер тела запроса (JSON с сообщением, без картинок).
pub const MAX_BODY_BYTES: usize = 64 * 1024;
/// Максимальный размер тела, если в запросе есть картинка: base64
/// примерно на треть длиннее исходных байтов плюс текст сообщения.
pub const MAX_IMAGE_BODY_BYTES: usize = vision::MAX_IMAGE_BYTES / 3 * 4 + MAX_BODY_BYTES + 4096;
/// Максимальная длина `message` в символах.
pub const MAX_MESSAGE_CHARS: usize = 16 * 1024;
/// Сколько ждём `SELECT 1` в `/v1/health`.
const HEALTH_DB_TIMEOUT: Duration = Duration::from_secs(2);
/// Период keep-alive-комментариев в SSE-потоке: прокси и браузеры
/// рвут «тихое» соединение, а комментарий `:` держит его живым.
const SSE_KEEPALIVE: Duration = Duration::from_secs(15);
/// Запас поверх `timeout_secs` агента для `TimeoutLayer`.
///
/// Своего таймаута у роутера быть не должно: `Assistant` уже ограничивает
/// каждый запрос апстрима значением `timeout_secs` из конфига, а медленная
/// локальная модель легко превышает любой захардкоженный лимит (200 с у
/// `bonsai-27b` — норма). Поэтому слой ставится с запасом, чтобы он ловил
/// только по-настоящему зависшие запросы, а не работающую модель.
const REQUEST_TIMEOUT_SLACK: Duration = Duration::from_secs(60);

/// Таймаут роутера: максимальный `timeout_secs` включённого агента плюс
/// [`REQUEST_TIMEOUT_SLACK`]. Считается на каждый запрос, поэтому
/// `set`/`reload` через control-сокет учитывается без рестарта.
fn request_timeout(cfg: &Config) -> Duration {
    cfg.agents
        .values()
        .filter(|a| a.enabled)
        .map(|a| Duration::from_secs(a.timeout_secs))
        .max()
        .unwrap_or(Duration::from_secs(ob_common::config::DEFAULT_TIMEOUT_SECS))
        + REQUEST_TIMEOUT_SLACK
}

/// Состояние, разделяемое между всеми хэндлерами.
#[derive(Clone)]
pub struct AppState {
    pub assistant: Arc<Assistant>,
    pub db: Database,
}

impl AppState {
    pub fn new(db: Database, cfg: Config) -> Self {
        AppState {
            db: db.clone(),
            assistant: Arc::new(Assistant::new(db, cfg)),
        }
    }
}

/// Корневой роутер. `/v1/health` открыт всегда, остальное — за токеном,
/// если он задан.
pub fn router(state: AppState) -> Router {
    // Лимит тела зависит от того, есть ли картинка, поэтому он внутри
    // хэндлеров, а не `RequestBodyLimitLayer` на всём роутере.
    let chat = Router::new()
        .route("/v1/chat/new", post(create_chat))
        .route("/v1/chat/message", post(post_message))
        .route("/v1/media/{message_id}", get(media))
        .route_layer(middleware::from_fn_with_state(state.clone(), require_token));
    Router::new()
        .route("/v1/health", get(health))
        .merge(chat)
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            request_timeout(&state.assistant.config()),
        ))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Проверка `Authorization: Bearer <token>`. Сравнение за постоянное
/// время, чтобы по таймингу нельзя было подбирать токен побайтно.
/// Токен берётся из актуального конфига, поэтому команда `set` сокета
/// может включить/сменить его без рестарта.
async fn require_token(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let cfg = state.assistant.config();
    let Some(expected) = cfg.api_token.as_deref() else {
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
    /// Картинка: data-URL (`data:image/png;base64,...`) или голый base64.
    #[serde(default)]
    pub image: Option<String>,
    /// `true` — сохранить картинку в БД (постоянный чат), иначе она
    /// останется во временном кэше.
    #[serde(default)]
    pub save_media: bool,
    /// `true` — ответ отдаётся потоком SSE.
    #[serde(default)]
    pub stream: bool,
}

/// Запрос на создание нового чата с первым сообщением.
#[derive(Debug, Deserialize)]
pub struct NewChatIn {
    pub user_id: i64,
    pub message: String,
    /// Ключ из `config.agents`; если не указан — агент с `main = true`.
    #[serde(default)]
    pub agent: Option<String>,
    /// Картинка: data-URL или голый base64 (см. [`MessageIn::image`]).
    #[serde(default)]
    pub image: Option<String>,
    /// `true` — сохранить картинку в БД.
    #[serde(default)]
    pub save_media: bool,
    /// `true` — ответ отдаётся потоком SSE.
    #[serde(default)]
    pub stream: bool,
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

/// Тело SSE-события стриминга.
#[derive(Debug, Serialize)]
#[serde(untagged)]
enum StreamOut {
    /// Очередная порция текста.
    Delta { delta: String },
    /// Финальное событие: та же полезная нагрузка, что у не-стримингового
    /// ответа, плюс флаг `done`.
    Done {
        chat_id: i64,
        time: i64,
        message: String,
        done: bool,
    },
    /// Ошибка на середине потока (или обрыв апстрима).
    Error { error: String },
}

/// Событие SSE: `data: {...}\n\n`. Ошибки сериализации невозможны
/// (структуры без хитрых типов), поэтому `unwrap_or_default`.
fn sse_event(payload: &StreamOut) -> Event {
    let data = serde_json::to_string(payload).unwrap_or_default();
    Event::default().data(data)
}

/// Разобрать необязательную картинку в `(VisionInput, save_media)`.
fn parse_image(image: Option<String>, save_media: bool) -> Result<Option<MediaInput>, AppError> {
    let Some(raw) = image else {
        return Ok(None);
    };
    if raw.trim().is_empty() {
        return Ok(None);
    }
    let parsed = VisionInput::parse(&raw).map_err(|e| AppError::BadRequest(e.to_string()))?;
    Ok(Some(MediaInput::new(parsed, save_media)))
}

/// Размер тела запроса ограничивается отдельно для картинок: `Json`
/// вычитывает тело целиком, поэтому без проверки большой base64 съел
/// бы память до всякой валидации.
async fn read_limited_body(req: Request, limit: usize) -> Result<bytes::Bytes, AppError> {
    axum::body::to_bytes(req.into_body(), limit)
        .await
        .map_err(|_| AppError::TooLarge(format!("request body is too large (max {limit} bytes)")))
}

// ---- Ошибки -------------------------------------------------------------

/// Тип ошибки для хэндлеров. `IntoResponse` маппит варианты в HTTP-статусы.
#[derive(Debug)]
pub enum AppError {
    BadRequest(String),
    /// Тело запроса больше разрешённого (413).
    TooLarge(String),
    NotFound(String),
    Core(crate::ObScapeError),
}

impl AppError {
    /// HTTP-статус для варианта ошибки; текст берётся из `Display`.
    fn status(&self) -> StatusCode {
        match self {
            AppError::BadRequest(_) => StatusCode::BAD_REQUEST,
            AppError::TooLarge(_) => StatusCode::PAYLOAD_TOO_LARGE,
            AppError::NotFound(_) => StatusCode::NOT_FOUND,
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
            AppError::BadRequest(m) | AppError::TooLarge(m) | AppError::NotFound(m) => {
                write!(f, "{m}")
            }
            AppError::Core(e) => write!(f, "{e}"),
        }
    }
}

impl From<crate::ObScapeError> for AppError {
    fn from(e: crate::ObScapeError) -> Self {
        AppError::Core(e)
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
///
/// `?stream=true` (или `"stream": true` в теле) → ответ уходит SSE-потоком.
async fn create_chat(State(state): State<AppState>, req: Request) -> Result<Response, AppError> {
    let wants_stream = is_stream_request(&req);
    let req: NewChatIn = read_json(req).await?;
    validate_message(&req.message)?;

    let media = parse_image(req.image, req.save_media)?;
    if wants_stream || req.stream {
        return Ok(stream_response(
            state
                .assistant
                .create_chat_stream(
                    req.user_id,
                    req.message,
                    req.agent,
                    media.into_iter().collect(),
                )
                .await?,
        ));
    }

    let (chat_id, reply) = state
        .assistant
        .create_chat(
            req.user_id,
            req.message,
            req.agent,
            media.into_iter().collect(),
        )
        .await?;

    Ok(Json(AssistantOut::new(chat_id, reply)).into_response())
}

/// `POST /v1/chat/message` — добавить сообщение в существующий чат.
async fn post_message(State(state): State<AppState>, req: Request) -> Result<Response, AppError> {
    let wants_stream = is_stream_request(&req);
    let req: MessageIn = read_json(req).await?;
    validate_message(&req.message)?;
    if req.chat_id <= 0 {
        return Err(AppError::BadRequest("chat_id must be positive".into()));
    }

    let media = parse_image(req.image, req.save_media)?;
    if wants_stream || req.stream {
        return Ok(stream_response(
            state
                .assistant
                .send_message_stream(
                    req.user_id,
                    req.chat_id,
                    req.message,
                    media.into_iter().collect(),
                )
                .await?,
        ));
    }

    let reply = state
        .assistant
        .send_message(
            req.user_id,
            req.chat_id,
            req.message,
            media.into_iter().collect(),
        )
        .await?;

    Ok(Json(AssistantOut::new(req.chat_id, reply)).into_response())
}

/// `GET /v1/media/{message_id}` — отдать картинку сообщения.
///
/// Сначала смотрим в БД (`save_media = true`), затем во временном кэше.
/// Так работают и «постоянные», и «временные» чаты.
async fn media(
    State(state): State<AppState>,
    Path(message_id): Path<i64>,
) -> Result<Response, AppError> {
    // 1. Картинка, сохранённая вместе с сообщением.
    if let Some(stored) = state
        .db
        .media_by_id(message_id)
        .await
        .map_err(crate::ObScapeError::Db)?
        && let Ok(bytes) = stored.decode()
    {
        return Ok(media_response(stored.mime, bytes));
    }

    // 2. Временный кэш.
    let id = message_id.to_string();
    let bytes = state
        .assistant
        .media_store()
        .load(&id)
        .ok_or_else(|| AppError::NotFound(format!("no media for message {message_id}")))?;
    Ok(media_response("application/octet-stream".into(), bytes))
}

/// Ответ с картинкой. MIME берём из БД, иначе отдаём как поток байтов
/// (браузер всё равно определит формат по сигнатуре).
fn media_response(mime: String, bytes: Vec<u8>) -> Response {
    let content_type = if vision::is_allowed_mime(&mime) {
        mime
    } else {
        "application/octet-stream".into()
    };
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, content_type),
            // Картинки неизменяемы: id — это хэш содержимого.
            (
                header::CACHE_CONTROL,
                "private, max-age=86400, immutable".to_string(),
            ),
        ],
        bytes,
    )
        .into_response()
}

/// Тело запроса читается с лимитом, который зависит от наличия `image`.
async fn read_json<T: serde::de::DeserializeOwned>(req: Request) -> Result<T, AppError> {
    let limit = request_body_limit(&req);
    let bytes = read_limited_body(req, limit).await?;
    serde_json::from_slice(&bytes)
        .map_err(|e| AppError::BadRequest(format!("invalid JSON body: {e}")))
}

/// Лимит тела: с картинкой он больше, чем для обычного JSON.
fn request_body_limit(req: &Request) -> usize {
    let has_image = req
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok())
        .is_some_and(|len| len > MAX_BODY_BYTES);
    if has_image {
        MAX_IMAGE_BODY_BYTES
    } else {
        MAX_BODY_BYTES
    }
}

/// Стрим запрошен либо заголовком `Accept: text/event-stream`, либо
/// query-параметром `?stream=true`.
fn is_stream_request(req: &Request) -> bool {
    if let Some(accept) = req
        .headers()
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        && accept.contains("text/event-stream")
    {
        return true;
    }
    req.uri().query().is_some_and(|q| {
        q.split('&')
            .any(|kv| matches!(kv, "stream=true" | "stream=1"))
    })
}

/// Обернуть поток событий [`Assistant`] в SSE-ответ.
///
/// Поток `Assistant` — единый: дельты текста, затем `Done` с id и полным
/// текстом (и уже записанным сообщением), либо ошибка. Здесь он просто
/// превращается в события `data: {...}`.
fn stream_response(mut events: ob_lib::ChatStream) -> Response {
    let stream = async_stream::stream! {
        while let Some(event) = events.next().await {
            let payload = match event {
                ob_lib::StreamEvent::Delta(delta) => match delta.strip_prefix('\u{0}') {
                    // Служебный маркер ошибки: текст потока и запись в БД
                    // не состоялись, клиенту уходит явная ошибка.
                    Some(rest) => StreamOut::Error { error: rest.to_string() },
                    None => StreamOut::Delta { delta },
                },
                ob_lib::StreamEvent::Done(out) => StreamOut::Done {
                    chat_id: out.chat_id,
                    time: out.time,
                    message: out.message,
                    done: true,
                },
            };
            yield Ok::<Event, Infallible>(sse_event(&payload));
        }
    };
    Sse::new(stream)
        .keep_alive(KeepAlive::new().interval(SSE_KEEPALIVE).text("keep-alive"))
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, header};
    use base64::Engine as _;
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
                backend: Default::default(),
                model_id: "test-model".into(),
                api_url: api_url.into(),
                api_key: "k".into(),
                personality_prompt: "p".into(),
                timeout_secs: 5,
                tools: HashMap::new(),
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
            control_socket: None,
            mcp_file: "mcp.json".into(),
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
    async fn api_token_follows_config_reload() {
        // Токен читается из актуального конфига: `set` через control-сокет
        // может включить аутентификацию без рестарта.
        let cfg = test_config("http://127.0.0.1:1/");
        let db = Database::connect_lazy(&cfg.database_url).expect("lazy pool");
        let state = AppState::new(db, cfg);
        let app = router(state.clone());
        let body = r#"{"user_id":1,"message":"hi"}"#;

        let (status, _) = call(app.clone(), post_json("/v1/chat/new", body.into())).await;
        assert_ne!(status, StatusCode::UNAUTHORIZED, "token not set yet");

        let mut with_token = state.assistant.config().as_ref().clone();
        with_token.api_token = Some("s3cret".into());
        state.assistant.replace_config(with_token).unwrap();

        let (status, json) = call(app, post_json("/v1/chat/new", body.into())).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{json}");
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

    #[test]
    fn request_timeout_follows_agent_config() {
        let cfg = test_config("http://127.0.0.1:1/");
        // 5 c из `test_config` + запас.
        assert_eq!(
            request_timeout(&cfg),
            Duration::from_secs(5) + REQUEST_TIMEOUT_SLACK
        );

        // Отключённый агент на таймаут не влияет: включённых нет, значит
        // берётся дефолт, а не 900 c и не 5 c отключённого.
        let mut slow = test_config("http://127.0.0.1:1/");
        slow.agents.get_mut("primary").unwrap().timeout_secs = 900;
        slow.agents.get_mut("primary").unwrap().enabled = false;
        assert_eq!(
            request_timeout(&slow),
            Duration::from_secs(ob_common::config::DEFAULT_TIMEOUT_SECS) + REQUEST_TIMEOUT_SLACK
        );

        // Максимум из нескольких включённых.
        let mut two = test_config("http://127.0.0.1:1/");
        let mut other = two.agents["primary"].clone();
        other.timeout_secs = 300;
        two.agents.insert("other".into(), other);
        assert_eq!(
            request_timeout(&two),
            Duration::from_secs(300) + REQUEST_TIMEOUT_SLACK
        );

        // Без агентов — дефолт.
        let mut empty = test_config("http://127.0.0.1:1/");
        empty.agents.clear();
        assert_eq!(
            request_timeout(&empty),
            Duration::from_secs(ob_common::config::DEFAULT_TIMEOUT_SECS) + REQUEST_TIMEOUT_SLACK
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

    /// Минимальный валидный PNG (важна только сигнатура) в base64.
    fn tiny_png_base64() -> String {
        let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        bytes.extend_from_slice(b"body");
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    #[tokio::test]
    async fn invalid_image_is_bad_request() {
        let body = serde_json::json!({
            "user_id": 1,
            "message": "hi",
            "image": "data:image/png;base64,not-base64!!",
        });
        let (status, json) = call(
            app("http://127.0.0.1:1/"),
            post_json("/v1/chat/new", body.to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            json["error"].as_str().unwrap().contains("invalid image"),
            "{json}"
        );
    }

    #[tokio::test]
    async fn oversized_image_body_is_rejected() {
        // Строка длиннее лимита для JSON без картинки — но с `image`
        // лимит выше, поэтому проверяем именно превышение image-лимита.
        let huge = "A".repeat(MAX_IMAGE_BODY_BYTES + 1024);
        let body = serde_json::json!({"user_id": 1, "message": "hi", "image": huge});
        let (status, _) = call(
            app("http://127.0.0.1:1/"),
            post_json("/v1/chat/new", body.to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn stream_flag_on_dead_upstream_is_bad_gateway_json() {
        // Соединение не устанавливается — хэндлер обязан ответить обычным
        // JSON-ом с 502, а не «начатым» SSE-потоком.
        let body = r#"{"user_id":1,"message":"hi","stream":true}"#;
        let (status, json) = call(
            app("http://127.0.0.1:1/"),
            post_json("/v1/chat/new", body.into()),
        )
        .await;
        assert_ne!(
            status,
            StatusCode::OK,
            "stream must not start on unreachable upstream: {json}"
        );
        assert!(json["error"].is_string(), "{json}");
    }

    #[tokio::test]
    async fn media_route_requires_token_and_reports_missing() {
        let mut cfg = test_config("http://127.0.0.1:1/");
        cfg.api_token = Some("s3cret".into());
        let req = Request::get("/v1/media/1").body(Body::empty()).unwrap();
        let (status, _) = call(app_with(cfg.clone()), req).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        // С токеном и живой БД медиа нет → 404.
        let Ok(db_url) = std::env::var("OBSCAPE_TEST_DATABASE_URL") else {
            eprintln!("OBSCAPE_TEST_DATABASE_URL is not set, skipping");
            return;
        };
        let db = Database::open_db(&db_url).await.expect("postgres");
        let app = router(AppState::new(db, cfg));
        let req = Request::get("/v1/media/999999999")
            .header(header::AUTHORIZATION, "Bearer s3cret")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn stream_true_body_field_is_accepted() {
        // `stream: true` в теле (без query/заголовка) тоже включает
        // стриминговый путь. Беда на любом шаге до/вместо потока должна
        // возвращаться обычным JSON-ом: тут — ленивый пул на закрытый
        // порт (или мёртвый апстрим), и наружу уходит 5xx с текстом,
        // а не начатый SSE-поток.
        let body = serde_json::json!({"user_id": 1, "message": "hi", "stream": true});
        let (status, json) = call(
            app("http://127.0.0.1:1/"),
            post_json("/v1/chat/new", body.to_string()),
        )
        .await;
        assert!(
            status.is_server_error(),
            "expected JSON 5xx, got {status}: {json}"
        );
        assert!(json["error"].is_string(), "{json}");
    }

    #[test]
    fn stream_detection_from_query_and_accept_header() {
        let req = Request::post("/v1/chat/new?stream=true")
            .body(Body::empty())
            .unwrap();
        assert!(is_stream_request(&req));

        let req = Request::post("/v1/chat/new?stream=1")
            .body(Body::empty())
            .unwrap();
        assert!(is_stream_request(&req));

        let req = Request::post("/v1/chat/new")
            .header(header::ACCEPT, "text/event-stream")
            .body(Body::empty())
            .unwrap();
        assert!(is_stream_request(&req));

        let req = Request::post("/v1/chat/new?foo=bar")
            .body(Body::empty())
            .unwrap();
        assert!(!is_stream_request(&req));
    }

    #[test]
    fn body_limit_grows_for_large_requests() {
        let small = Request::post("/v1/chat/new").body(Body::empty()).unwrap();
        assert_eq!(request_body_limit(&small), MAX_BODY_BYTES);

        let big = Request::post("/v1/chat/new")
            .header(header::CONTENT_LENGTH, (MAX_BODY_BYTES + 1).to_string())
            .body(Body::empty())
            .unwrap();
        assert_eq!(request_body_limit(&big), MAX_IMAGE_BODY_BYTES);
    }

    #[test]
    fn stream_out_serializes_delta_and_done() {
        let delta = serde_json::to_value(StreamOut::Delta { delta: "hi".into() }).unwrap();
        assert_eq!(delta, serde_json::json!({"delta": "hi"}));

        let done = serde_json::to_value(StreamOut::Done {
            chat_id: 3,
            time: 42,
            message: "full".into(),
            done: true,
        })
        .unwrap();
        assert_eq!(
            done,
            serde_json::json!({"chat_id": 3, "time": 42, "message": "full", "done": true})
        );

        let error = serde_json::to_value(StreamOut::Error {
            error: "boom".into(),
        })
        .unwrap();
        assert_eq!(error, serde_json::json!({"error": "boom"}));
    }

    #[test]
    fn parse_image_accepts_data_url_and_bare_base64() {
        let b64 = tiny_png_base64();
        let alt = parse_image(Some(format!("data:image/png;base64,{b64}")), true).unwrap();
        assert!(alt.unwrap().save);

        let bare = parse_image(Some(b64), false).unwrap().unwrap();
        assert!(!bare.save);
        assert_eq!(bare.image.mime, "image/png");

        assert!(parse_image(None, false).unwrap().is_none());
        assert!(parse_image(Some("  ".into()), false).unwrap().is_none());
        assert!(parse_image(Some("garbage".into()), false).is_err());
    }
}
