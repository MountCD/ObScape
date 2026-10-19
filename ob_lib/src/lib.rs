pub use ob_common;
use ob_common::agent;
use ob_common::config::Config;
use ob_common::database::{ContentStruc, Database, JsonMessageContent, Roles};
use ob_common::time::now_iso;
use ob_common::vlog;
use std::sync::{Arc, RwLock};

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
    /// Актуальный конфиг. Хранится под блокировкой, чтобы control-сокет
    /// мог подгрузить конфиг на лету (`reload` / `set`), а хэндлеры в
    /// каждый момент работали со снимком одного консистентного значения.
    cfg: RwLock<Arc<Config>>,
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
            cfg: RwLock::new(Arc::new(cfg)),
            http: Arc::new(http),
        }
    }

    /// Снимок текущего конфига. Возвращается `Arc`, чтобы вызывающий
    /// код мог спокойно работать с ним, пока идёт запрос к апстриму,
    /// даже если в этот момент конфиг перезагрузили.
    pub fn config(&self) -> Arc<Config> {
        self.cfg.read().expect("config lock poisoned").clone()
    }

    /// Применить новый конфиг. Пул соединений создаётся один раз при
    /// старте, поэтому смена `database_url` отклоняется — для неё нужен
    /// рестарт.
    pub fn replace_config(&self, cfg: Config) -> Result<(), ObScapeError> {
        let current = self.config();
        if cfg.database_url != current.database_url {
            return Err(ObScapeError::BadRequest(
                "database_url cannot be changed at runtime (restart required)".into(),
            ));
        }
        *self.cfg.write().expect("config lock poisoned") = Arc::new(cfg);
        Ok(())
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
        let cfg = self.config();
        // Агент не указан — берём `main = true` из конфига.
        let agent = match agent {
            Some(a) => a,
            None => cfg.main_agent().map(str::to_string).ok_or_else(|| {
                ObScapeError::BadRequest(
                    "agent is not specified and no main agent is configured".into(),
                )
            })?,
        };
        // Проверяем агента до того, как что-либо записывать в БД.
        let a_cfg = agent::resolve_agent(&cfg, &agent).map_err(ObScapeError::Llm)?;

        // Чат и его системный промпт (shared + personality) создаются
        // одной транзакцией.
        let now = now_iso();
        let sys_prompt = a_cfg.merge_config(&cfg.shared_prompt);
        let res_chat = ob_common::vdbg!(
            &cfg,
            self.db
                .create_chat(user_id, &agent, &now, &sys_prompt)
                .await
        );
        let chat_id = res_chat.map_err(ObScapeError::Db)?;
        vlog!(&cfg, "chat {chat_id} created for agent `{agent}`");

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
        let cfg = self.config();
        vlog!(
            &cfg,
            "processing message for user {user_id}, chat {chat_id}"
        );

        // 0. Агент, закреплённый за чатом. Заодно проверяем, что чат
        //    принадлежит этому пользователю.
        let res_agent = ob_common::vdbg!(&cfg, self.db.chat_agent_for_user(chat_id, user_id).await);
        let agent = res_agent
            .map_err(ObScapeError::Db)?
            .ok_or_else(|| ObScapeError::BadRequest(format!("unknown chat_id: {chat_id}")))?;
        let a_cfg = agent::resolve_agent(&cfg, &agent).map_err(ObScapeError::Llm)?;

        // 1. Save user message
        let now = now_iso();
        let res_save = ob_common::vdbg!(
            &cfg,
            self.db
                .add_message(JsonMessageContent::new(
                    Roles::User,
                    ContentStruc::new(now, chat_id, user_id, message.clone()),
                ))
                .await
        );
        let user_msg_id = res_save.map_err(ObScapeError::Db)?;

        // 2. Get history and call LLM. Если модель не ответила — убираем
        //    только что сохранённую реплику, чтобы повтор запроса не дал
        //    двух `user` подряд в истории.
        let reply = match self.request_reply(&cfg, chat_id, &a_cfg).await {
            Ok(r) => r,
            Err(e) => {
                if let Err(del) = self.db.delete_message(user_msg_id).await {
                    vlog!(
                        &cfg,
                        "failed to roll back user message {user_msg_id}: {del}"
                    );
                }
                return Err(e);
            }
        };

        // 3. Save assistant reply
        let reply_time = now_iso();
        let res_reply = ob_common::vdbg!(
            &cfg,
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

    /// История чата → апстрим. Вынесено, чтобы `send_message` мог
    /// откатить реплику пользователя при любой ошибке этого шага.
    /// `cfg` — снимок, с которым работает весь `send_message`.
    async fn request_reply(
        &self,
        cfg: &Config,
        chat_id: i64,
        a_cfg: &ob_common::config::AgentConfig,
    ) -> Result<String, ObScapeError> {
        let res_history = ob_common::vdbg!(
            cfg,
            self.db.export_chat_recent(chat_id, cfg.history_limit).await
        );
        let history = res_history.map_err(ObScapeError::Db)?;
        agent::make_request_with(&self.http, cfg, a_cfg, &history)
            .await
            .map_err(ObScapeError::Llm)
    }

    pub fn check_tools_call() {
        todo!()
    }

    /// Заготовка под вызов инструментов: ещё не подключена к обработчикам.
    #[allow(dead_code)]
    async fn use_tool() {
        todo!()
    }
}
