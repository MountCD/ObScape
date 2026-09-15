pub use ob_common;
use ob_common::agent;
use ob_common::config::Config;
use ob_common::database::{ContentStruc, Database, JsonMessageContent, Roles};
use ob_common::time::now_iso;
use ob_common::vlog;
use std::sync::Arc;

#[derive(Debug)]
pub enum ObScapeError {
    Db(sqlx::Error),
    Llm(ob_common::agent::AgentError),
    BadRequest(String),
    Internal(String),
}

impl std::fmt::Display for ObScapeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ObScapeError::Db(e) => write!(f, "db error: {e}"),
            ObScapeError::Llm(e) => write!(f, "llm error: {e}"),
            ObScapeError::BadRequest(m) | ObScapeError::Internal(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for ObScapeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ObScapeError::Db(e) => Some(e),
            ObScapeError::Llm(e) => Some(e),
            _ => None,
        }
    }
}

pub struct Assistant {
    db: Arc<Database>,
    cfg: Arc<Config>,
    http: Arc<reqwest::Client>,
}

impl Assistant {
    pub fn new(db: Database, cfg: Config) -> Self {
        // Общий таймаут запроса задаётся per-agent в `make_request_with`;
        // здесь — страховочный дефолт и таймаут на установку соединения.
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(
                ob_common::config::DEFAULT_TIMEOUT_SECS,
            ))
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .expect("failed to build HTTP client");
        Self {
            db: Arc::new(db),
            cfg: Arc::new(cfg),
            http: Arc::new(http),
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
        agent: Option<String>,
    ) -> Result<(i64, String), ObScapeError> {
        // Агент не указан — берём `main = true` из конфига.
        let agent = match agent {
            Some(a) => a,
            None => self
                .cfg
                .main_agent()
                .map(str::to_string)
                .ok_or_else(|| {
                    ObScapeError::BadRequest(
                        "agent is not specified and no main agent is configured".into(),
                    )
                })?,
        };
        // Проверяем агента до того, как что-либо записывать в БД.
        let a_cfg = agent::resolve_agent(&self.cfg, &agent).map_err(ObScapeError::Llm)?;

        let now = now_iso();
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

        // 0. Агент, закреплённый за чатом. Заодно проверяем, что чат
        //    принадлежит этому пользователю.
        let res_agent = ob_common::vdbg!(
            &*self.cfg,
            self.db.chat_agent_for_user(chat_id, user_id).await
        );
        let agent = res_agent
            .map_err(ObScapeError::Db)?
            .ok_or_else(|| ObScapeError::BadRequest(format!("unknown chat_id: {chat_id}")))?;
        let a_cfg = agent::resolve_agent(&self.cfg, &agent).map_err(ObScapeError::Llm)?;

        // 1. Save user message
        let now = now_iso();
        let res_save = ob_common::vdbg!(
            &*self.cfg,
            self.db
                .add_message(JsonMessageContent::new(
                    Roles::User,
                    ContentStruc::new(now, chat_id, user_id, message.clone()),
                ))
                .await
        );
        res_save.map_err(ObScapeError::Db)?;

        // 2. Get history and call LLM
        let res_history = ob_common::vdbg!(
            &*self.cfg,
            self.db
                .export_chat_recent(chat_id, self.cfg.history_limit)
                .await
        );
        let history = res_history.map_err(ObScapeError::Db)?;
        let reply = agent::make_request_with(&self.http, &self.cfg, &a_cfg, &history)
            .await
            .map_err(ObScapeError::Llm)?;

        // 3. Save assistant reply
        let reply_time = now_iso();
        let res_reply = ob_common::vdbg!(
            &*self.cfg,
            self.db
                .add_message(JsonMessageContent::new(
                    Roles::Assistant,
                    ContentStruc::new(reply_time, chat_id, user_id, reply.clone()),
                ))
                .await
        );
        res_reply.map_err(ObScapeError::Db)?;

        Ok(reply)
    }
}
