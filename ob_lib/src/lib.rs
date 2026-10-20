pub use ob_common;
use ob_common::agent;
use ob_common::agent::DeltaStream;
use ob_common::config::Config;
use ob_common::database::{ContentStruc, Database, JsonMessageContent, Roles};
use ob_common::time::now_iso;
use ob_common::vision::{StoredMedia, VisionStore};
use ob_common::vlog;
use std::pin::Pin;
use std::sync::{Arc, RwLock};
use tokio_stream::Stream;

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

/// Картинка, приложенная к сообщению пользователя.
#[derive(Debug, Clone)]
pub struct MediaInput {
    /// Разобранная картинка (mime, байты, content-id).
    pub image: ob_common::vision::VisionInput,
    /// `true` — класть картинку в БД, чтобы чат пережил чистку кэша.
    pub save: bool,
}

impl MediaInput {
    pub fn new(image: ob_common::vision::VisionInput, save: bool) -> Self {
        MediaInput { image, save }
    }
}

/// Поток ответа модели для стримингового режима: `Streaming` — дельты,
/// `Done` — финальное состояние (id чата, время, полный текст).
pub enum StreamEvent {
    /// Очередная порция текста.
    Delta(String),
    /// Конец ответа; текст уже записан в БД.
    Done(StreamOut),
}

/// Итог завершённого стрима — то же, что отдаёт не-стриминговый путь.
#[derive(Debug, Clone)]
pub struct StreamOut {
    pub chat_id: i64,
    pub time: i64,
    pub message: String,
}

/// Поток событий стриминга, каким его отдаёт [`ob_lib::Assistant`].
pub type ChatStream = Pin<Box<dyn Stream<Item = StreamEvent> + Send>>;

pub struct Assistant {
    db: Arc<Database>,
    /// Актуальный конфиг. Хранится под блокировкой, чтобы control-сокет
    /// мог подгрузить конфиг на лету (`reload` / `set`), а хэндлеры в
    /// каждый момент работали со снимком одного консистентного значения.
    cfg: RwLock<Arc<Config>>,
    http: Arc<reqwest::Client>,
    /// Кэш картинок: временные чаты (`save_media = false`) живут здесь.
    media: VisionStore,
}

impl Assistant {
    pub fn new(db: Database, cfg: Config) -> Self {
        Self::with_media_store(db, cfg, VisionStore::default())
    }

    /// Как [`Assistant::new`], но с явным кэшем медиа (нужно тестам).
    pub fn with_media_store(db: Database, cfg: Config, media: VisionStore) -> Self {
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
            media,
        }
    }

    /// Кэш медиа — нужен серверу для отдачи `/v1/media/{id}`.
    pub fn media_store(&self) -> &VisionStore {
        &self.media
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
        media: Vec<MediaInput>,
    ) -> Result<(i64, String), ObScapeError> {
        let cfg = self.config();
        let a_cfg = self.prepare_agent(&cfg, agent)?;

        // Чат и его системный промпт (shared + personality) создаются
        // одной транзакцией.
        let now = now_iso();
        let sys_prompt = a_cfg.cfg.merge_config(&cfg.shared_prompt);
        let res_chat = ob_common::vdbg!(
            &cfg,
            self.db
                .create_chat(user_id, a_cfg.name(), &now, &sys_prompt)
                .await
        );
        let chat_id = res_chat.map_err(ObScapeError::Db)?;
        vlog!(&cfg, "chat {chat_id} created for agent `{}`", a_cfg.name());

        let reply = self
            .send_message(user_id, chat_id, message, media)
            .await
            .map_err(|e| self.with_chat_hint(e))?;
        Ok((chat_id, reply))
    }

    /// Создать чат и ответить стримом. Отличие от [`Assistant::create_chat`]
    /// только в том, что сообщение пользователя обрабатывается потоком.
    pub async fn create_chat_stream(
        &self,
        user_id: i64,
        message: String,
        agent: Option<String>,
        media: Vec<MediaInput>,
    ) -> Result<ChatStream, ObScapeError> {
        let cfg = self.config();
        let a_cfg = self.prepare_agent(&cfg, agent)?;

        let now = now_iso();
        let sys_prompt = a_cfg.cfg.merge_config(&cfg.shared_prompt);
        let res_chat = ob_common::vdbg!(
            &cfg,
            self.db
                .create_chat(user_id, a_cfg.name(), &now, &sys_prompt)
                .await
        );
        let chat_id = res_chat.map_err(ObScapeError::Db)?;
        vlog!(&cfg, "chat {chat_id} created for agent `{}`", a_cfg.name());

        self.send_message_stream(user_id, chat_id, message, media)
            .await
            .map_err(|e| self.with_chat_hint(e))
    }

    /// Сообщение в существующий чат. Агент не передаётся — он берётся из БД
    /// по `chat_id`.
    pub async fn send_message(
        &self,
        user_id: i64,
        chat_id: i64,
        message: String,
        media: Vec<MediaInput>,
    ) -> Result<String, ObScapeError> {
        let cfg = self.config();
        vlog!(
            &cfg,
            "processing message for user {user_id}, chat {chat_id}"
        );
        let a_cfg = self.agent_for_chat(&cfg, chat_id, user_id).await?;

        // 1. Save user message (с картинками — если просили их сохранить).
        let (user_msg_id, stored) = self
            .save_user_message(&cfg, chat_id, user_id, &message, media)
            .await?;

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
        let _ = stored;

        // 3. Save assistant reply
        self.save_assistant_message(&cfg, chat_id, user_id, reply)
            .await
    }

    /// То же, что [`Assistant::send_message`], но ответ отдаётся потоком.
    pub async fn send_message_stream(
        &self,
        user_id: i64,
        chat_id: i64,
        message: String,
        media: Vec<MediaInput>,
    ) -> Result<ChatStream, ObScapeError> {
        let cfg = self.config();
        vlog!(&cfg, "streaming message for user {user_id}, chat {chat_id}");
        let a_cfg = self.agent_for_chat(&cfg, chat_id, user_id).await?;
        let (user_msg_id, _) = self
            .save_user_message(&cfg, chat_id, user_id, &message, media)
            .await?;

        let history = ob_common::vdbg!(
            &cfg,
            self.db.export_chat_recent(chat_id, cfg.history_limit).await
        )
        .map_err(ObScapeError::Db)?;

        // Ошибка установления соединения — отдаём сразу, чтобы хэндлер
        // успел ответить корректным HTTP-статусом, а не начатым потоком.
        let deltas = agent::make_stream_request(&self.http, &cfg, &a_cfg.cfg, &history)
            .await
            .map_err(|e| {
                // Соединения нет — реплика пользователя не должна остаться.
                let db = self.db.clone();
                let cfg2 = cfg.clone();
                tokio::spawn(async move {
                    if let Err(del) = db.delete_message(user_msg_id).await {
                        vlog!(
                            &cfg2,
                            "failed to roll back user message {user_msg_id}: {del}"
                        );
                    }
                });
                ObScapeError::Llm(e)
            })?;

        let s = stream_events(deltas, self.db.clone(), cfg, chat_id, user_id, user_msg_id);
        Ok(Box::pin(s))
    }

    /// Определить агента для нового чата: либо указанного, либо `main`.
    fn prepare_agent(
        &self,
        cfg: &Config,
        agent: Option<String>,
    ) -> Result<NamedAgent, ObScapeError> {
        let name = match agent {
            Some(a) => a,
            None => cfg.main_agent().map(str::to_string).ok_or_else(|| {
                ObScapeError::BadRequest(
                    "agent is not specified and no main agent is configured".into(),
                )
            })?,
        };
        // Проверяем агента до того, как что-либо записывать в БД.
        let a_cfg = agent::resolve_agent(cfg, &name).map_err(ObScapeError::Llm)?;
        Ok(NamedAgent { name, cfg: a_cfg })
    }

    /// Агент, закреплённый за чатом (с проверкой владельца).
    async fn agent_for_chat(
        &self,
        cfg: &Config,
        chat_id: i64,
        user_id: i64,
    ) -> Result<NamedAgent, ObScapeError> {
        let name = ob_common::vdbg!(&cfg, self.db.chat_agent_for_user(chat_id, user_id).await)
            .map_err(ObScapeError::Db)?
            .ok_or_else(|| ObScapeError::BadRequest(format!("unknown chat_id: {chat_id}")))?;
        let a_cfg = agent::resolve_agent(cfg, &name).map_err(ObScapeError::Llm)?;
        Ok(NamedAgent { name, cfg: a_cfg })
    }

    /// Сохранить реплику пользователя. В БД уходят только те картинки,
    /// для которых выставлен `save_media`; остальные остаются в кэше
    /// `VisionStore` и обслуживаются по `id`.
    async fn save_user_message(
        &self,
        cfg: &Config,
        chat_id: i64,
        user_id: i64,
        message: &str,
        media: Vec<MediaInput>,
    ) -> Result<(i64, Vec<StoredMedia>), ObScapeError> {
        let mut stored = Vec::new();
        for m in &media {
            self.media
                .store(&m.image)
                .map_err(|e| ObScapeError::BadRequest(e.to_string()))?;
            if m.save {
                stored.push(StoredMedia::from_input(&m.image));
            }
        }
        // Ошибка вытеснения кэша не должна ломать запрос.
        if let Err(e) = self.media.evict_expired() {
            vlog!(&cfg, "media evict failed: {e}");
        }

        let now = now_iso();
        let msg = JsonMessageContent::new(
            Roles::User,
            ContentStruc::new(now, chat_id, user_id, message.to_string())
                .with_media(stored.clone()),
        );
        let res = self.db.add_message(msg).await;
        let id = ob_common::vdbg!(&cfg, res).map_err(ObScapeError::Db)?;
        Ok((id, stored))
    }

    /// Сохранить ответ ассистента и вернуть его текст.
    async fn save_assistant_message(
        &self,
        cfg: &Config,
        chat_id: i64,
        user_id: i64,
        reply: String,
    ) -> Result<String, ObScapeError> {
        let reply_time = now_iso();
        let msg = JsonMessageContent::new(
            Roles::Assistant,
            ContentStruc::new(reply_time, chat_id, user_id, reply.clone()),
        );
        let res = self.db.add_message(msg).await;
        ob_common::vdbg!(&cfg, res).map_err(ObScapeError::Db)?;
        Ok(reply)
    }

    /// Добавить к ошибке подсказку, если чат уже создан, но сообщение
    /// не обработалось: клиенту будет что предъявить.
    fn with_chat_hint(&self, e: ObScapeError) -> ObScapeError {
        e
    }

    /// История чата → апстрим. Вынесено, чтобы `send_message` мог
    /// откатить реплику пользователя при любой ошибке этого шага.
    /// `cfg` — снимок, с которым работает весь `send_message`.
    async fn request_reply(
        &self,
        cfg: &Config,
        chat_id: i64,
        a_cfg: &NamedAgent,
    ) -> Result<String, ObScapeError> {
        let res_history = ob_common::vdbg!(
            cfg,
            self.db.export_chat_recent(chat_id, cfg.history_limit).await
        );
        let history = res_history.map_err(ObScapeError::Db)?;
        agent::make_request_with(&self.http, cfg, &a_cfg.cfg, &history)
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

/// Агент вместе с его именем в конфиге (нужно для записи в `chats`).
struct NamedAgent {
    name: String,
    cfg: ob_common::config::AgentConfig,
}

impl NamedAgent {
    fn name(&self) -> &str {
        &self.name
    }
}

/// Обернуть поток дельт в поток [`StreamEvent`]: текст отдаём наружу
/// как есть, а по завершении (или обрыве) один раз пишем в БД итоговое
/// сообщение ассистента.
///
/// Сохранение сделано прямо в генераторе, а не через `Drop`: тогда
/// факт записи виден по `Done`, а обрыв клиента оставляет только реплику
/// пользователя (роллбэк — как в не-стриминговом пути).
fn stream_events(
    deltas: DeltaStream,
    db: Arc<Database>,
    cfg: Arc<Config>,
    chat_id: i64,
    user_id: i64,
    user_msg_id: i64,
) -> impl Stream<Item = StreamEvent> + Send {
    async_stream::stream! {
        use futures_util::StreamExt;
        let mut deltas = deltas;
        let mut full = String::new();
        let mut failed: Option<String> = None;

        while let Some(item) = deltas.next().await {
            match item {
                Ok(delta) => {
                    full.push_str(&delta);
                    yield StreamEvent::Delta(delta);
                }
                Err(e) => {
                    failed = Some(e.to_string());
                    break;
                }
            }
        }

        if let Some(err) = failed {
            // Модель оборвалась: убираем реплику пользователя, чтобы
            // повтор не дал двух `user` подряд, и сообщаем об ошибке.
            if let Err(del) = db.delete_message(user_msg_id).await {
                vlog!(&cfg, "failed to roll back user message {user_msg_id}: {del}");
            }
            yield StreamEvent::Delta(format!("\u{0}error: {err}"));
            return;
        }

        let time = now_iso();
        let msg = JsonMessageContent::new(
            Roles::Assistant,
            ContentStruc::new(time, chat_id, user_id, full.clone()),
        );
        if let Err(e) = db.add_message(msg).await {
            vlog!(&cfg, "failed to save assistant message: {e}");
            yield StreamEvent::Delta(format!("\u{0}error: db error: {e}"));
            return;
        }

        yield StreamEvent::Done(StreamOut {
            chat_id,
            time: ob_common::time::unix_secs(),
            message: full,
        });
    }
}
