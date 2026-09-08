pub use ob_common;
use ob_common::agent;
use ob_common::config::{AgentConfig, Config};
use ob_common::database::{ContentStruc, Database, JsonMessageContent, Roles};
use ob_common::vlog;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug)]
pub enum ObScapeError {
    Db(sqlx::Error),
    Llm(ob_common::agent::AgentError),
    Internal(String),
}

pub struct Assistant {
    db: Arc<Database>,
    a_cfg: Arc<AgentConfig>,
    s_cfg: Arc<Config>,
    http: Arc<reqwest::Client>,
}

impl Assistant {
    pub fn new(db: Database, cfg: AgentConfig) -> Self {
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
        vlog!(
            &*self.cfg,
            "processing message for user {user_id}, chat {chat_id}"
        );

        let now = unix_secs();

        // 1. Save user message
        let res_save = ob_common::vdbg!(
            &*self.cfg,
            self.db
                .add_message(JsonMessageContent::new(
                    Roles::User,
                    ContentStruc::new(iso_from_unix(now), chat_id, user_id, message.clone()),
                ))
                .await
        );
        res_save.map_err(ObScapeError::Db)?;

        // 2. Get history and call LLM
        let res_history = ob_common::vdbg!(&*self.cfg, self.db.export_chat(chat_id).await);
        let mut history = res_history.map_err(ObScapeError::Db)?;
        let reply = agent::make_request_with(&self.http, &self.cfg, &mut history, message)
            .await
            .map_err(ObScapeError::Llm)?;

        // 3. Save assistant reply
        let reply_time = unix_secs();
        let res_reply = ob_common::vdbg!(
            &*self.cfg,
            self.db
                .add_message(JsonMessageContent::new(
                    Roles::Assistant,
                    ContentStruc::new(iso_from_unix(reply_time), chat_id, user_id, reply.clone()),
                ))
                .await
        );
        res_reply.map_err(ObScapeError::Db)?;

        Ok(reply)
    }

    pub async fn create_chat(
        &self,
        chat_id: i64,
        user_id: i64,
        message: String,
        agent: String,
    ) -> Result<(i64, String), ObScapeError> {
        let chat_id = self.next_chat_id().await?;
        let reply = self.send_sys_prompt(chat_id, agent).await?;
        Ok((chat_id, reply))
    }

    async fn next_chat_id(&self) -> Result<i64, ObScapeError> {
        let row: (Option<i64>,) = sqlx::query_as(r#"SELECT MAX(chat_id) FROM messages"#)
            .fetch_one(self.db.pool())
            .await
            .map_err(ObScapeError::Db)?;
        Ok(row.0.unwrap_or(0).saturating_add(1))
    }

    async fn send_sys_prompt(&self, chat_id: i64, agent: String) {
        let sys_prompt = self.cfg.merge_config(shared)
        let reply = agent::make_request(&self.http, message, agent);
        todo!()
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
