// PostgreSQL-backed message store.
use crate::vision::StoredMedia;
use serde::{Deserialize, Serialize};
use sqlx::postgres::{PgPool, PgPoolOptions};

/// Сообщение, хранимое в БД и используемое при формировании LLM-запроса.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Roles {
    System,
    User,
    Assistant,
    Tool,
}

impl Roles {
    /// Строковое представление роли для записи в Postgres (`system` / `user` / `assistant`).
    pub fn as_str(&self) -> &'static str {
        match self {
            Roles::System => "system",
            Roles::User => "user",
            Roles::Assistant => "assistant",
            Roles::Tool => "tool",
        }
    }

    /// Обратное преобразование `as_str` -> `Roles`.
    fn role_from_str(s: &str) -> Result<Self, String> {
        match s {
            "system" => Ok(Roles::System),
            "user" => Ok(Roles::User),
            "assistant" => Ok(Roles::Assistant),
            "tool" => Ok(Roles::Tool),
            other => Err(format!("unknown role: {other}")),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String, // всегда "function"
    pub function: FunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String, // JSON внутри строки, как отдаёт API
}

/// Сообщение в формате апстрима.
///
/// `content` — либо простая строка (обычный текст), либо массив частей
/// `[{type: "text"}, {type: "image_url"}]`: второй вариант нужен для
/// vision, и именно его ждут OpenAI-совместимые серверы. Сериализация
/// ветвится по [`LlmContent`] (`#[serde(untagged)]`), наружу формат
/// остаётся прежним для текстовых сообщений.
#[derive(Debug, Serialize, Clone)]
pub struct LlmMessage {
    pub role: Roles,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<LlmContentBody>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

/// Содержимое сообщения: строка или массив частей (text/image_url).
#[derive(Debug, Serialize, Clone)]
#[serde(untagged)]
pub enum LlmContentBody {
    /// Обычное текстовое сообщение — уходит строкой, как раньше.
    Text(String),
    /// Мультимодальное сообщение — уходит массивом частей.
    Parts(Vec<crate::vision::LlmContent>),
}

impl LlmContentBody {
    /// Только текст (для Ollama-ветки и для проверок).
    pub fn as_text(&self) -> Option<&str> {
        match self {
            LlmContentBody::Text(s) => Some(s),
            LlmContentBody::Parts(_) => None,
        }
    }

    /// Картинки в виде data-URL; пустой вектор — сообщение текстовое.
    pub fn data_urls(&self) -> Vec<String> {
        match self {
            LlmContentBody::Text(_) => Vec::new(),
            LlmContentBody::Parts(parts) => parts
                .iter()
                .filter_map(|p| match p {
                    crate::vision::LlmContent::ImageUrl { image_url } => {
                        Some(image_url.url.clone())
                    }
                    crate::vision::LlmContent::Text { .. } => None,
                })
                .collect(),
        }
    }
}

impl From<&str> for LlmContentBody {
    fn from(s: &str) -> Self {
        LlmContentBody::Text(s.to_string())
    }
}

impl From<String> for LlmContentBody {
    fn from(s: String) -> Self {
        LlmContentBody::Text(s)
    }
}

/// Собрать содержимое сообщения из текста и картинок.
///
/// Без картинок получается обычная строка (обратная совместимость),
/// с картинками — массив частей: сначала текст, потом `image_url`.
pub fn message_content(text: &str, images: &[StoredMedia]) -> Option<LlmContentBody> {
    if images.is_empty() {
        return (!text.is_empty()).then(|| LlmContentBody::Text(text.to_string()));
    }
    let mut parts = Vec::with_capacity(images.len() + 1);
    if !text.is_empty() {
        parts.push(crate::vision::LlmContent::text(text));
    }
    parts.extend(
        images
            .iter()
            .map(|m| crate::vision::LlmContent::image_data_url(m.to_data_url())),
    );
    Some(LlmContentBody::Parts(parts))
}

impl From<&JsonMessageContent> for LlmMessage {
    fn from(m: &JsonMessageContent) -> Self {
        let c = &m.content;
        match &m.role {
            // ответ модели с запросом инструментов
            Roles::Assistant if c.tool_calls.is_some() => LlmMessage {
                role: Roles::Assistant,
                content: (!c.message.is_empty()).then(|| c.message.clone().into()),
                tool_calls: c.tool_calls.clone(),
                tool_call_id: None,
            },
            // результат работы инструмента
            Roles::Tool => LlmMessage {
                role: Roles::Tool,
                content: Some(c.message.clone().into()),
                tool_calls: None,
                tool_call_id: c.tool_call_id.clone(),
            },
            // обычные system / user / assistant
            role => LlmMessage {
                role: role.clone(),
                content: message_content(&c.message, &c.media),
                tool_calls: None,
                tool_call_id: None,
            },
        }
    }
}

#[derive(Debug, Serialize, Clone)]
pub struct JsonRequestMessage {
    model: String,
    messages: Vec<LlmMessage>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<StreamOptions>,
}

/// Опции стрима. Включаем `include_usage`, чтобы провайдеры, которые
/// его требуют для совместимости, не отдавали ошибку на `stream: true`.
#[derive(Debug, Serialize, Clone)]
pub struct StreamOptions {
    pub include_usage: bool,
}

impl JsonRequestMessage {
    pub fn new(model: String, messages: Vec<LlmMessage>, stream: bool) -> Self {
        JsonRequestMessage {
            model,
            messages,
            stream,
            stream_options: stream.then_some(StreamOptions {
                include_usage: false,
            }),
        }
    }
}

#[derive(Debug, Serialize, Clone, Deserialize)]
pub struct ContentStruc {
    time: String,
    chat_id: i64,
    user_id: i64,
    message: String,
    #[serde(default)]
    tool_calls: Option<Vec<ToolCall>>, // только у assistant
    #[serde(default)]
    tool_call_id: Option<String>, // только у tool
    /// Картинки пользователя (`save_media = true`). Пусто — сообщение
    /// только текстовое. `skip_serializing_if` держит JSON-представление
    /// прежним, пока медиа нет.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    media: Vec<StoredMedia>,
}

impl ContentStruc {
    pub fn new(time: String, chat_id: i64, user_id: i64, message: String) -> Self {
        ContentStruc {
            time,
            chat_id,
            user_id,
            message,
            tool_calls: None,
            tool_call_id: None,
            media: Vec::new(),
        }
    }

    /// То же сообщение, но с прикреплёнными картинками.
    pub fn with_media(mut self, media: Vec<StoredMedia>) -> Self {
        self.media = media;
        self
    }

    /// Картинки сообщения.
    pub fn media(&self) -> &[StoredMedia] {
        &self.media
    }
}

#[derive(Debug, Serialize, Clone, Deserialize)]
pub struct JsonMessageContent {
    role: Roles,
    content: ContentStruc,
}

impl JsonMessageContent {
    pub fn new(role: Roles, content: ContentStruc) -> Self {
        JsonMessageContent { role, content }
    }
}

/// Обёртка над пулом соединений PostgreSQL.
///
/// Хранит `PgPool` (он `Clone`/`Send`/`Sync`), методы — асинхронные.
#[derive(Debug, Clone)]
pub struct Database {
    pool: PgPool,
}

impl Database {
    /// Открыть пул без подключения и без создания таблиц — соединение
    /// устанавливается при первом запросе. Нужно для тестов хэндлеров,
    /// которые не доходят до БД; для сервера используйте `open_db`.
    pub fn connect_lazy(url: &str) -> Result<Self, sqlx::Error> {
        let pool = PgPoolOptions::new().max_connections(1).connect_lazy(url)?;
        Ok(Database { pool })
    }

    /// Открыть (создать) базу сообщений по строке подключения.
    ///
    /// `url` — обычный Postgres URL, например:
    /// `postgres://user:pass@localhost:5432/obscape`.
    /// Таблицы `messages` и `chats` создаются автоматически, если их ещё нет.
    pub async fn open_db(url: &str) -> Result<Self, sqlx::Error> {
        let pool = PgPoolOptions::new().max_connections(8).connect(url).await?;

        // Авто-создание таблицы при первом запуске.
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS messages (
                id           BIGSERIAL PRIMARY KEY,
                "time"       TEXT     NOT NULL,
                chat_id      BIGINT   NOT NULL,
                user_id      BIGINT   NOT NULL,
                role         TEXT     NOT NULL,
                message      TEXT    ,
                tool_calls   TEXT    ,
                tool_call_id TEXT    
            )
            "#,
        )
        .execute(&pool)
        .await?;

        // Чаты: агент закрепляется за чатом один раз, при создании.
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS chats (
                chat_id    BIGSERIAL PRIMARY KEY,
                user_id    BIGINT  NOT NULL,
                agent      TEXT    NOT NULL,
                created_at TEXT    NOT NULL
            )
            "#,
        )
        .execute(&pool)
        .await?;

        // Индекс для быстрых выборок по чату/времени.
        sqlx::query(
            r#"
            CREATE INDEX IF NOT EXISTS messages_chat_time_idx
                ON messages (chat_id, "time")
            "#,
        )
        .execute(&pool)
        .await?;

        // Картинки пользователя (vision, `save_media = true`). Для баз,
        // созданных до появления vision, колонки ещё нет — `ADD COLUMN
        // IF NOT EXISTS` доводит схему до актуальной без ручной миграции.
        sqlx::query(r#"ALTER TABLE messages ADD COLUMN IF NOT EXISTS media TEXT"#)
            .execute(&pool)
            .await?;

        Ok(Database { pool })
    }

    /// Прочитать сообщения конкретного чата.
    pub async fn export_chat(&self, chat_id: i64) -> Result<Vec<JsonMessageContent>, sqlx::Error> {
        let rows = sqlx::query_as::<_, MessageRow>(
            r#"
            SELECT "time", chat_id, user_id, role, message, tool_calls, tool_call_id, media
            FROM messages
            WHERE chat_id = $1
            ORDER BY "time" ASC, id ASC
            "#,
        )
        .bind(chat_id)
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(MessageRow::into_message).collect()
    }

    /// Прочитать системные сообщения чата плюс последние `limit` обычных —
    /// то, что реально уходит модели. Порядок хронологический.
    pub async fn export_chat_recent(
        &self,
        chat_id: i64,
        limit: u32,
    ) -> Result<Vec<JsonMessageContent>, sqlx::Error> {
        let rows = sqlx::query_as::<_, MessageRow>(
            r#"
            SELECT "time", chat_id, user_id, role, message, tool_calls, tool_call_id, media
            FROM (
                SELECT id, "time", chat_id, user_id, role, message, tool_calls, tool_call_id, media
                FROM messages
                WHERE chat_id = $1 AND role = 'system'
                UNION ALL
                SELECT id, "time", chat_id, user_id, role, message, tool_calls, tool_call_id, media
                FROM (
                    SELECT id, "time", chat_id, user_id, role, message, tool_calls, tool_call_id, media
                    FROM messages
                    WHERE chat_id = $1 AND role <> 'system'
                    ORDER BY "time" DESC, id DESC
                    LIMIT $2
                ) AS recent
            ) AS selected
            ORDER BY "time" ASC, id ASC
            "#,
        )
        .bind(chat_id)
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(MessageRow::into_message).collect()
    }

    /// Добавить одно сообщение. Возвращает `id` строки — по нему сообщение
    /// можно откатить через [`Database::delete_message`].
    ///
    /// Картинки (`content.media`) сохраняются в БД только здесь и только
    /// в том случае, если вызывающий код их туда положил (это делает
    /// `Assistant` при `save_media = true`).
    pub async fn add_message(&self, message: JsonMessageContent) -> Result<i64, sqlx::Error> {
        let calls = message
            .content
            .tool_calls
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| sqlx::Error::Encode(Box::new(e)))?;
        let media = (!message.content.media.is_empty())
            .then(|| serde_json::to_string(&message.content.media))
            .transpose()
            .map_err(|e| sqlx::Error::Encode(Box::new(e)))?;
        let text =
            (!message.content.message.is_empty()).then_some(message.content.message.as_str());

        let row: (i64,) = sqlx::query_as(
            r#"
            INSERT INTO messages ("time", chat_id, user_id, role, message, tool_calls, tool_call_id, media)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            RETURNING id
            "#,
        )
        .bind(message.content.time.as_str())
        .bind(message.content.chat_id)
        .bind(message.content.user_id)
        .bind(message.role.as_str())
        .bind(text)
        .bind(calls)
        .bind(message.content.tool_call_id.as_deref())
        .bind(media)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.0)
    }

    /// Картинки конкретного сообщения по его `id` (роут `/v1/media/{id}`):
    /// сначала из БД (`save_media = true`), иначе — из кэша [`VisionStore`].
    pub async fn media_by_id(&self, message_id: i64) -> Result<Option<StoredMedia>, sqlx::Error> {
        let row: Option<(Option<String>,)> =
            sqlx::query_as(r#"SELECT media FROM messages WHERE id = $1"#)
                .bind(message_id)
                .fetch_optional(&self.pool)
                .await?;
        let Some((Some(raw),)) = row else {
            return Ok(None);
        };
        let stored: Vec<StoredMedia> = serde_json::from_str(&raw)
            .map_err(|e| sqlx::Error::Protocol(format!("invalid media in DB row: {e}")))?;
        Ok(stored.into_iter().next())
    }

    /// Удалить сообщение по `id`. Используется как компенсация: если
    /// апстрим не ответил, реплика пользователя не должна оставаться в
    /// истории (иначе в контексте появятся два `user` подряд).
    pub async fn delete_message(&self, id: i64) -> Result<(), sqlx::Error> {
        sqlx::query(r#"DELETE FROM messages WHERE id = $1"#)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Создать новый чат, закрепить за ним агента и записать системный
    /// промпт — одной транзакцией, чтобы не оставалось чатов без промпта.
    ///
    /// Возвращает выданный `chat_id`. Имя агента после этого меняться не может —
    /// все последующие сообщения чата обслуживает именно он.
    pub async fn create_chat(
        &self,
        user_id: i64,
        agent: &str,
        created_at: &str,
        system_prompt: &str,
    ) -> Result<i64, sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        let row: (i64,) = sqlx::query_as(
            r#"
            INSERT INTO chats (user_id, agent, created_at)
            VALUES ($1, $2, $3)
            RETURNING chat_id
            "#,
        )
        .bind(user_id)
        .bind(agent)
        .bind(created_at)
        .fetch_one(&mut *tx)
        .await?;
        let chat_id = row.0;
        sqlx::query(
            r#"
            INSERT INTO messages ("time", chat_id, user_id, role, message)
            VALUES ($1, $2, $3, $4, $5)
            "#,
        )
        .bind(created_at)
        .bind(chat_id)
        .bind(user_id)
        .bind(Roles::System.as_str())
        .bind(system_prompt)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(chat_id)
    }

    /// Имя агента, закреплённого за чатом. `None` — чата не существует
    /// или он принадлежит другому пользователю (наружу это не различается).
    pub async fn chat_agent_for_user(
        &self,
        chat_id: i64,
        user_id: i64,
    ) -> Result<Option<String>, sqlx::Error> {
        let row: Option<(String,)> =
            sqlx::query_as(r#"SELECT agent FROM chats WHERE chat_id = $1 AND user_id = $2"#)
                .bind(chat_id)
                .bind(user_id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.map(|r| r.0))
    }

    /// Проверка живости БД для `/v1/health`.
    pub async fn ping(&self) -> Result<(), sqlx::Error> {
        sqlx::query("SELECT 1").execute(&self.pool).await?;
        Ok(())
    }
}

/// Внутренняя строка результата `SELECT ... FROM messages`.
#[derive(sqlx::FromRow)]
struct MessageRow {
    time: String,
    chat_id: i64,
    user_id: i64,
    role: String,
    message: Option<String>,
    tool_calls: Option<String>,
    tool_call_id: Option<String>,
    media: Option<String>,
}

impl MessageRow {
    fn into_message(self) -> Result<JsonMessageContent, sqlx::Error> {
        let role = Roles::role_from_str(&self.role)
            .map_err(|e| sqlx::Error::Protocol(format!("invalid role in DB row: {e}")))?;
        let tool_calls = self
            .tool_calls
            .as_deref()
            .map(serde_json::from_str::<Vec<ToolCall>>)
            .transpose()
            .map_err(|e| sqlx::Error::Protocol(format!("invalid tool_calls in DB row: {e}")))?;
        let media = self
            .media
            .as_deref()
            .map(serde_json::from_str::<Vec<StoredMedia>>)
            .transpose()
            .map_err(|e| sqlx::Error::Protocol(format!("invalid media in DB row: {e}")))?
            .unwrap_or_default();
        Ok(JsonMessageContent {
            role,
            content: ContentStruc {
                time: self.time,
                chat_id: self.chat_id,
                user_id: self.user_id,
                message: self.message.unwrap_or_default(),
                tool_calls,
                tool_call_id: self.tool_call_id,
                media,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::Roles;

    #[test]
    fn role_round_trip() {
        for role in [Roles::System, Roles::User, Roles::Assistant, Roles::Tool] {
            let back = Roles::role_from_str(role.as_str()).unwrap();
            assert_eq!(back.as_str(), role.as_str());
        }
    }

    #[test]
    fn unknown_role_is_rejected() {
        assert!(Roles::role_from_str("abcd123").is_err());
    }

    #[test]
    fn role_serializes_as_snake_case() {
        assert_eq!(
            serde_json::to_string(&Roles::Assistant).unwrap(),
            "\"assistant\""
        );
    }
}
