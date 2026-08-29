// PostgreSQL-backed message store.
use serde::{Deserialize, Serialize};
use sqlx::postgres::{PgPool, PgPoolOptions};

/// Сообщение, хранимое в БД и используемое при формировании LLM-запроса.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Roles {
    System,
    User,
    Assistant,
}

impl Roles {
    /// Строковое представление роли для записи в Postgres (`system` / `user` / `assistant`).
    pub fn as_str(&self) -> &'static str {
        match self {
            Roles::System => "system",
            Roles::User => "user",
            Roles::Assistant => "assistant",
        }
    }

    /// Обратное преобразование `as_str` -> `Roles`.
    pub fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "system" => Ok(Roles::System),
            "user" => Ok(Roles::User),
            "assistant" => Ok(Roles::Assistant),
            other => Err(format!("unknown role: {other}")),
        }
    }
}

#[derive(Debug, Serialize, Clone)]
pub struct JsonRequestMessage {
    model: String,
    messages: Vec<JsonMessageContent>,
    stream: bool,
}

impl JsonRequestMessage {
    pub fn new(model: String, messages: Vec<JsonMessageContent>, stream: bool) -> Self {
        JsonRequestMessage {
            model,
            messages,
            stream,
        }
    }
}

#[derive(Debug, Serialize, Clone, Deserialize)]
pub struct ContentStruc {
    time: String,
    chat_id: i64,
    user_id: i64,
    message: String,
}

impl ContentStruc {
    pub fn new(time: String, chat_id: i64, user_id: i64, message: String) -> Self {
        ContentStruc {
            time,
            chat_id,
            user_id,
            message,
        }
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
    /// Открыть (создать) базу сообщений по строке подключения.
    ///
    /// `url` — обычный Postgres URL, например:
    /// `postgres://user:pass@localhost:5432/obsistent`.
    /// Таблица `messages` создаётся автоматически, если её ещё нет.
    pub async fn open_db(url: &str) -> Result<Self, sqlx::Error> {
        let pool = PgPoolOptions::new()
            .max_connections(8)
            .connect(url)
            .await?;

        // Авто-создание таблицы при первом запуске.
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS messages (
                id        BIGSERIAL PRIMARY KEY,
                "time"    TEXT    NOT NULL,
                chat_id   BIGINT  NOT NULL,
                user_id   BIGINT  NOT NULL,
                role      TEXT    NOT NULL,
                message   TEXT    NOT NULL
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

        Ok(Database { pool })
    }

    /// В прежней версии сбрасывал JSON-файл. В Postgres это не нужно —
    /// оставлено как no-op для совместимости по сигнатуре.
    pub async fn write_db(&self) -> Result<(), sqlx::Error> {
        Ok(())
    }

    /// Прочитать все сообщения из БД, упорядоченные по `chat_id` и `time`.
    pub async fn export_messages(&self) -> Result<Vec<JsonMessageContent>, sqlx::Error> {
        let rows = sqlx::query_as::<_, MessageRow>(
            r#"
            SELECT "time", chat_id, user_id, role, message
            FROM messages
            ORDER BY chat_id ASC, "time" ASC, id ASC
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter().map(MessageRow::into_message).collect()
    }

    /// Прочитать сообщения конкретного чата.
    pub async fn export_chat(
        &self,
        chat_id: i64,
    ) -> Result<Vec<JsonMessageContent>, sqlx::Error> {
        let rows = sqlx::query_as::<_, MessageRow>(
            r#"
            SELECT "time", chat_id, user_id, role, message
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

    /// Добавить одно сообщение.
    pub async fn add_message(&self, message: JsonMessageContent) -> Result<(), sqlx::Error> {
        sqlx::query(
            r#"
            INSERT INTO messages ("time", chat_id, user_id, role, message)
            VALUES ($1, $2, $3, $4, $5)
            "#,
        )
        .bind(message.content.time.as_str())
        .bind(message.content.chat_id)
        .bind(message.content.user_id)
        .bind(message.role.as_str())
        .bind(message.content.message.as_str())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Пакетная вставка сообщений одной транзакцией.
    pub async fn add_messages(
        &self,
        messages: &[JsonMessageContent],
    ) -> Result<(), sqlx::Error> {
        if messages.is_empty() {
            return Ok(());
        }
        let mut tx = self.pool.begin().await?;
        for m in messages {
            sqlx::query(
                r#"
                INSERT INTO messages ("time", chat_id, user_id, role, message)
                VALUES ($1, $2, $3, $4, $5)
                "#,
            )
            .bind(m.content.time.as_str())
            .bind(m.content.chat_id)
            .bind(m.content.user_id)
            .bind(m.role.as_str())
            .bind(m.content.message.as_str())
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Доступ к пулу — для случаев, когда нужны произвольные запросы.
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}

/// Внутренняя строка результата `SELECT ... FROM messages`.
#[derive(sqlx::FromRow)]
struct MessageRow {
    time: String,
    chat_id: i64,
    user_id: i64,
    role: String,
    message: String,
}

impl MessageRow {
    fn into_message(self) -> Result<JsonMessageContent, sqlx::Error> {
        let role = Roles::from_str(&self.role)
            .map_err(|e| sqlx::Error::Protocol(format!("invalid role in DB row: {e}")))?;
        Ok(JsonMessageContent {
            role,
            content: ContentStruc {
                time: self.time,
                chat_id: self.chat_id,
                user_id: self.user_id,
                message: self.message,
            },
        })
    }
}
