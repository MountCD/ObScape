use serde::Deserialize;
use std::collections::HashMap;
use std::os::unix::net::{UnixListener,UnixStream}; 

#[derive(Debug, Deserialize, Clone)]
pub struct AgentConfig {
    pub enabled: bool,
    /// Агент по умолчанию: используется, когда в `POST /v1/chat/new`
    /// не передан `agent`. Не больше одного включённого агента с `main = true`.
    #[serde(default)]
    pub main: bool,
    pub model_id: String,
    pub api_url: String,
    /// Ключ апстрима. Можно не указывать в TOML и задать через
    /// `OBSISTENT_AGENT_<ИМЯ>_API_KEY` (env имеет приоритет). Пустая
    /// строка считается отсутствием ключа.
    #[serde(default)]
    pub api_key: String,
    pub personality_prompt: String,
    /// Таймаут одного запроса к апстриму в секундах (разным моделям
    /// нужно разное время). Дефолт — `DEFAULT_TIMEOUT_SECS`.
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
    /// Список всех доступных для использования инструментов, описанных в mcp.json.
    pub tools: HashMap<String, ToolRights> // String на время, в будущем тут будет сам Tool.
}
#[derive(Debug, Deserialize, Clone, Copy)]
pub enum ToolRights {
    Approve,
    Ask,
    Deny
}

/// Дефолт для `timeout_secs`, если поле не задано в config.toml.
pub const DEFAULT_TIMEOUT_SECS: u64 = 120;

fn default_timeout_secs() -> u64 {
    DEFAULT_TIMEOUT_SECS
}
impl AgentConfig {
    /// Системный промпт чата: общий + личный, через пустую строку,
    /// чтобы многострочные промпты не слипались.
    pub fn merge_config(&self, shared: &str) -> String {
        format!("{}\n\n{}", shared, self.personality_prompt)
    }
}
#[derive(Debug, Deserialize, Clone)]
pub struct Config {

    pub database_url: String,
    /// Адрес и порт HTTP-сервера (например, `0.0.0.0:11080`).
    /// `None` означает "использовать дефолт".
    pub http_bind: Option<String>,
    pub shared_prompt: String,
    /// Сколько последних сообщений (без учёта системного промпта)
    /// отправлять модели. Ограничивает рост запроса на длинных чатах.
    #[serde(default = "default_history_limit")]
    pub history_limit: u32,
    /// Bearer-токен для `/v1/chat/*`. `None` — без аутентификации
    /// (только для локальной разработки). Env `OBSISTENT_API_TOKEN`
    /// имеет приоритет.
    #[serde(default)]
    pub api_token: Option<String>,
    /// Сюда указывается файл mcp.json, содержащий все доступные инструменты.
    pub mcp_file: String,
    pub agents: HashMap<String, AgentConfig>,
}

impl Config {
    /// Ключ включённого агента с `main = true`, если такой есть.
    pub fn main_agent(&self) -> Option<&str> {
        self.agents
            .iter()
            .find(|(_, a)| a.enabled && a.main)
            .map(|(name, _)| name.as_str())
    }
}

/// Дефолт для `history_limit`, если поле не задано в config.toml.
pub const DEFAULT_HISTORY_LIMIT: u32 = 50;

fn default_history_limit() -> u32 {
    DEFAULT_HISTORY_LIMIT
}
