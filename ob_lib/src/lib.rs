pub use ob_common;
use ob_common::agent;
use ob_common::config::Config;
use ob_common::database::{ContentStruc, Database, JsonMessageContent, Roles};
use ob_common::vlog;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug)]
pub enum ObScapeError {
    Db(sqlx::Error),
    Llm(ob_common::agent::AgentError),
    BadRequest(String),
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

    /// Создать новый чат: агент указывается только здесь и закрепляется за чатом.
    ///
    /// Порядок: резолвим агента из конфига → заводим строку в `chats` →
    /// пишем системный промпт (shared + personality) → обрабатываем первое
    /// сообщение пользователя обычным путём.
    pub async fn create_chat(
        &self,
        user_id: i64,
        message: String,
        agent: String,
    ) -> Result<(i64, String), ObScapeError> {
        // Проверяем агента до того, как что-либо записывать в БД.
        let a_cfg = agent::resolve_agent(&self.cfg, &agent).map_err(ObScapeError::Llm)?;

        let now = iso_from_unix(unix_secs());
        let res_chat = ob_common::vdbg!(
            &*self.cfg,
            self.db.create_chat(user_id, &agent, &now).await
        );
        let chat_id = res_chat.map_err(ObScapeError::Db)?;
        vlog!(&*self.cfg, "chat {chat_id} created for agent `{agent}`");

        // Системный промпт пишется один раз, при создании чата.
        let sys_prompt = a_cfg.merge_config(&self.cfg.shared_prompt);
        let res_sys = ob_common::vdbg!(
            &*self.cfg,
            self.db
                .add_message(JsonMessageContent::new(
                    Roles::System,
                    ContentStruc::new(now, chat_id, user_id, sys_prompt),
                ))
                .await
        );
        res_sys.map_err(ObScapeError::Db)?;

        let reply = self.send_message(user_id, chat_id, message).await?;
        Ok((chat_id, reply))
    }

    /// Сообщение в существующий чат. Агент не передаётся — он берётся из БД
    /// по `chat_id`.
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

        // 0. Агент, закреплённый за чатом.
        let res_agent = ob_common::vdbg!(&*self.cfg, self.db.chat_agent(chat_id).await);
        let agent = res_agent
            .map_err(ObScapeError::Db)?
            .ok_or_else(|| ObScapeError::BadRequest(format!("unknown chat_id: {chat_id}")))?;
        let a_cfg = agent::resolve_agent(&self.cfg, &agent).map_err(ObScapeError::Llm)?;

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
        let reply = agent::make_request_with(&self.http, &self.cfg, &a_cfg, &mut history, message)
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
