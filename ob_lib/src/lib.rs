pub use ob_common;
use ob_common::config::Config;
use ob_common::database::{ContentStruc, Database, JsonMessageContent, Roles};
use ob_common::llm;
use ob_common::llm::Models;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug)]
pub enum ObScapeError {
    Db(sqlx::Error),
    Llm(ob_common::llm::LlmError),
    Internal(String),
}

pub struct Assistant {
    db: Arc<Database>,
    cfg: Arc<Config>,
    http: Arc<reqwest::Client>,
}

impl Assistant {
    pub fn new(db: Database, cfg: Config) -> Self {
        Self {
            db: Arc::new(db),
            cfg: Arc::new(cfg),
            http: Arc::new(reqwest::Client::new()),
        }
    }

    pub async fn send_message(
        &self,
        user_id: i64,
        chat_id: i64,
        message: String,
    ) -> Result<String, ObScapeError> {
        let now = unix_secs();

        // 1. Save user message
        self.db
            .add_message(JsonMessageContent::new(
                Roles::User,
                ContentStruc::new(iso_from_unix(now), chat_id, user_id, message.clone()),
            ))
            .await
            .map_err(ObScapeError::Db)?;

        // 2. Get history and call LLM
        let mut history = self
            .db
            .export_chat(chat_id)
            .await
            .map_err(ObScapeError::Db)?;
        let reply = llm::make_request_with(
            &self.http,
            &self.cfg,
            Models::TalkModel, // Simplified for now
            &mut history,
            message,
        )
        .await
        .map_err(ObScapeError::Llm)?;

        // 3. Save assistant reply
        let reply_time = unix_secs();
        self.db
            .add_message(JsonMessageContent::new(
                Roles::Assistant,
                ContentStruc::new(iso_from_unix(reply_time), chat_id, user_id, reply.clone()),
            ))
            .await
            .map_err(ObScapeError::Db)?;

        Ok(reply)
    }

    pub async fn create_chat(
        &self,
        user_id: i64,
        message: String,
    ) -> Result<(i64, String), ObScapeError> {
        let chat_id = self.next_chat_id().await?;
        let reply = self.send_message(user_id, chat_id, message).await?;
        Ok((chat_id, reply))
    }

    async fn next_chat_id(&self) -> Result<i64, ObScapeError> {
        let row: (Option<i64>,) = sqlx::query_as(r#"SELECT MAX(chat_id) FROM messages"#)
            .fetch_one(self.db.pool())
            .await
            .map_err(ObScapeError::Db)?;
        Ok(row.0.unwrap_or(0).saturating_add(1))
    }
}

fn unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn iso_from_unix(_secs: i64) -> String {
    "1970-01-01T00:00:00Z".to_string()
}
