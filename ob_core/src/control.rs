//! Unix-сокет управления: подгрузка конфига на лету.
//!
//! Протокол — одна JSON-команда на соединение, одна строка:
//!
//! ```text
//! {"cmd":"ping"}                       -> {"ok":true,"service":"obscape"}
//! {"cmd":"get"}                        -> {"ok":true,"config":"<TOML с масками>"}
//! {"cmd":"reload"}                     -> {"ok":true}
//! {"cmd":"set","config":"<TOML>"}      -> {"ok":true}
//! ```
//!
//! Ответ — одна строка JSON: `{"ok":true,...}` либо
//! `{"ok":false,"error":"..."}`. Сокет создаётся с правами `0600`:
//! читать и писать может только владелец процесса.

use ob_common::config::{self, Config, PathOverrides};
use ob_lib::Assistant;
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

/// Максимальный размер одной команды: в `set` уходит весь конфиг,
/// поэтому лимит щедрый, но ограничивает болтливость клиента.
const MAX_CMD_BYTES: u64 = 1024 * 1024;

/// Всё, что нужно для обработки команд сокета.
#[derive(Clone)]
pub struct ControlState {
    pub assistant: Arc<Assistant>,
    /// Переопределения CLI, с которыми стартовал процесс: `reload`/`set`
    /// перечитывают **тот же файл**, что и стартовая загрузка.
    pub overrides: PathOverrides,
    /// Файл `config.toml`: его читает `reload` и в него пишет `set`.
    pub config_path: PathBuf,
    /// Реально занятый путь сокета (значение `control_socket` или
    /// `<каталог конфига>/control.sock`).
    pub socket_path: PathBuf,
    /// Адрес, на который уже забинден HTTP-сервер.
    pub http_bind: String,
}

/// Входящая команда. Тег — значение поля `cmd`.
#[derive(Debug, Deserialize)]
#[serde(tag = "cmd", rename_all = "lowercase")]
enum Command {
    Ping,
    Get,
    Reload,
    Set { config: String },
}

/// Привязать сокет, подготовив его к работе:
/// - существующий, но мёртвый (после падения процесса) файл удаляется;
/// - живой слушатель — ошибка `AddrInUse`, второй сервер не поднимется;
/// - права файла выставляются в `0600`.
pub fn bind(state: &ControlState) -> std::io::Result<UnixListener> {
    let path = &state.socket_path;
    if path.exists() {
        match std::os::unix::net::UnixStream::connect(path) {
            Ok(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AddrInUse,
                    format!("another process is already listening on {}", path.display()),
                ));
            }
            Err(_) => {
                // Лежащий мёртвый сокет от прошлого запуска — убираем.
                let _ = std::fs::remove_file(path);
            }
        }
    }
    if let Some(dir) = path.parent()
        && !dir.as_os_str().is_empty()
        && !dir.exists()
    {
        std::fs::create_dir_all(dir)?;
    }
    let listener = UnixListener::bind(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(listener)
}

/// Слушать сокет до остановки процесса. Каждое соединение обрабатывается
/// в отдельной задаче: клиент может не закрывать соединение, не мешая остальным.
pub async fn serve(listener: UnixListener, state: ControlState) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let state = state.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_connection(stream, state).await {
                        tracing::warn!("control socket: connection failed: {e}");
                    }
                });
            }
            Err(e) => {
                tracing::error!("control socket: accept failed: {e}");
                break;
            }
        }
    }
}

async fn handle_connection(stream: UnixStream, state: ControlState) -> std::io::Result<()> {
    let (read, mut write) = stream.into_split();
    let mut line = String::new();
    BufReader::new(read)
        .take(MAX_CMD_BYTES)
        .read_line(&mut line)
        .await?;

    let reply = if line.trim().is_empty()
        || (!line.ends_with('\n') && line.len() as u64 >= MAX_CMD_BYTES)
    {
        json!({"ok": false, "error": "empty or oversized command (one JSON per line, max 1 MiB)"})
    } else {
        dispatch(&state, line.trim_end())
    };

    let mut out = serde_json::to_string(&reply)
        .unwrap_or_else(|_| r#"{"ok":false,"error":"failed to serialize reply"}"#.to_string());
    out.push('\n');
    write.write_all(out.as_bytes()).await?;
    write.shutdown().await
}

/// Разобрать и выполнить команду. Вынесен отдельно от сокета, чтобы
/// тесты могли гонять команды напрямую.
pub(crate) fn dispatch(state: &ControlState, body: &str) -> Value {
    match serde_json::from_str::<Command>(body) {
        Err(e) => json!({"ok": false, "error": format!("invalid command: {e}")}),
        Ok(cmd) => run(state, cmd),
    }
}

fn run(state: &ControlState, cmd: Command) -> Value {
    match cmd {
        Command::Ping => json!({"ok": true, "service": "obscape"}),
        Command::Get => match state.assistant.config().masked_toml() {
            Ok(text) => json!({"ok": true, "config": text}),
            Err(e) => json!({"ok": false, "error": e}),
        },
        Command::Reload => match config::reload_config(&state.overrides) {
            Ok(cfg) => apply(state, cfg).map_or_else(
                |e| json!({"ok": false, "error": e}),
                |_| json!({"ok": true}),
            ),
            Err(e) => json!({"ok": false, "error": e.to_string()}),
        },
        Command::Set { config: text } => set_config(state, &text),
    }
}

/// `set`: записать присланный конфиг в файл, перечитать и применить.
/// При любой ошибке файл возвращается к прежнему содержимому.
fn set_config(state: &ControlState, text: &str) -> Value {
    let old = std::fs::read_to_string(&state.config_path).ok();
    if let Err(e) = std::fs::write(&state.config_path, text) {
        return json!({"ok": false, "error": format!("failed to write {}: {e}", state.config_path.display())});
    }

    let rollback = |reason: String| -> Value {
        match &old {
            Some(prev) => {
                if let Err(e) = std::fs::write(&state.config_path, prev) {
                    tracing::error!(
                        "control socket: failed to roll back {}: {e}",
                        state.config_path.display()
                    );
                }
            }
            None => {
                let _ = std::fs::remove_file(&state.config_path);
            }
        }
        json!({"ok": false, "error": reason})
    };

    match config::reload_config(&state.overrides) {
        Err(e) => rollback(format!("invalid config: {e}")),
        Ok(cfg) => {
            if let Err(e) = check_appliable(state, &cfg) {
                return rollback(e);
            }
            match state.assistant.replace_config(cfg) {
                Ok(()) => json!({"ok": true}),
                Err(e) => rollback(e.to_string()),
            }
        }
    }
}

/// `reload`/`set`: проверить, что новый конфиг вообще применим к
/// запущенному процессу, и заменить его.
fn apply(state: &ControlState, cfg: Config) -> Result<(), String> {
    check_appliable(state, &cfg)?;
    state
        .assistant
        .replace_config(cfg)
        .map_err(|e| e.to_string())
}

/// Часть конфига, которая уже закреплена за запущенным процессом и не
/// может измениться без рестарта. Проверяется до записи в `Assistant`,
/// чтобы отклонённый конфиг не остался «наполовину применённым».
fn check_appliable(state: &ControlState, cfg: &Config) -> Result<(), String> {
    if looks_masked(cfg) {
        return Err(
            "config contains masked values (\"***\"): `get` output is for inspection only, \
             send the real config instead"
                .into(),
        );
    }
    let current = state.assistant.config();
    if cfg.database_url != current.database_url {
        return Err("database_url cannot be changed at runtime (restart required)".into());
    }
    let new_bind = cfg
        .http_bind
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(config::DEFAULT_HTTP_BIND)
        .to_string();
    if new_bind != state.http_bind {
        return Err(format!(
            "http_bind cannot be changed at runtime (restart required, server is bound to {})",
            state.http_bind
        ));
    }
    let new_socket = effective_socket_path(state, cfg);
    if new_socket != state.socket_path {
        return Err(format!(
            "control_socket cannot be changed at runtime (restart required, server listens on {})",
            state.socket_path.display()
        ));
    }
    Ok(())
}

/// Какой путь сокета имел бы данный конфиг: явный `control_socket` или
/// `<каталог config.toml>/control.sock`, как это считает `main`.
fn effective_socket_path(state: &ControlState, cfg: &Config) -> PathBuf {
    if let Some(p) = cfg
        .control_socket
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return PathBuf::from(p);
    }
    state
        .config_path
        .parent()
        .map(|dir| dir.join("control.sock"))
        .unwrap_or_else(|| PathBuf::from("control.sock"))
}

/// Открытые секреты из `get` — признак того, что клиент прислал
/// замаскированный конфиг обратно.
fn looks_masked(cfg: &Config) -> bool {
    cfg.database_url.contains("***")
        || cfg.api_token.as_deref() == Some("***")
        || cfg.agents.values().any(|a| a.api_key == "***")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ob_common::database::Database;
    use std::time::UNIX_EPOCH;

    /// Временный каталог, удаляющийся за собой.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let dir = std::env::temp_dir()
                .join(format!("obscape-control-{}-{nanos}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }

        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const BASE_CONFIG: &str = r#"
database_url = "postgres://user:s3cret@localhost:5432/obscape"
http_bind = "0.0.0.0:11080"
shared_prompt = "shared v1"
history_limit = 50
mcp_file = "mcp.json"

[agents.primary]
enabled = true
main = true
backend = "openai"
model_id = "m"
api_url = "https://api.example.com/v1/chat/completions"
api_key = "k"
personality_prompt = "p"
timeout_secs = 10
tools = {}
"#;

    fn state_in(dir: &TempDir) -> ControlState {
        std::fs::write(dir.path().join("config.toml"), BASE_CONFIG).unwrap();
        let overrides = PathOverrides::with_config_dir(dir.path().to_str().unwrap());
        let cfg = config::reload_config(&overrides).expect("load test config");
        let db = Database::connect_lazy(&cfg.database_url).expect("lazy pool");
        ControlState {
            assistant: Arc::new(Assistant::new(db, cfg)),
            overrides,
            config_path: dir.path().join("config.toml"),
            socket_path: dir.path().join("control.sock"),
            http_bind: "0.0.0.0:11080".into(),
        }
    }

    fn cmd(state: &ControlState, body: &str) -> Value {
        dispatch(state, body)
    }

    #[tokio::test]
    async fn ping_and_invalid_commands() {
        let dir = TempDir::new();
        let state = state_in(&dir);

        assert_eq!(cmd(&state, r#"{"cmd":"ping"}"#)["ok"], true);
        assert_eq!(cmd(&state, "not json at all")["ok"], false);
        assert_eq!(cmd(&state, r#"{"cmd":"explode"}"#)["ok"], false);
        assert_eq!(cmd(&state, "")["ok"], false);
    }

    #[tokio::test]
    async fn get_returns_masked_toml() {
        let dir = TempDir::new();
        let state = state_in(&dir);

        let reply = cmd(&state, r#"{"cmd":"get"}"#);
        assert_eq!(reply["ok"], true, "{reply}");
        let text = reply["config"].as_str().expect("config toml");
        assert!(!text.contains("s3cret"), "password leaked: {text}");
        assert!(!text.contains("\"k\""), "api_key leaked: {text}");
        assert!(text.contains("user:***@"), "{text}");
        assert!(text.contains("history_limit"), "{text}");

        // Замаскированный конфиг не принимается обратно.
        let reply = cmd(&state, &json!({"cmd": "set", "config": text}).to_string());
        assert_eq!(reply["ok"], false, "{reply}");
        assert!(
            reply["error"].as_str().unwrap().contains("masked values"),
            "{reply}"
        );
    }

    #[tokio::test]
    async fn reload_picks_up_file_changes() {
        let dir = TempDir::new();
        let state = state_in(&dir);

        std::fs::write(
            state.config_path.clone(),
            BASE_CONFIG.replace("shared v1", "shared v2"),
        )
        .unwrap();
        let reply = cmd(&state, r#"{"cmd":"reload"}"#);
        assert_eq!(reply["ok"], true, "{reply}");
        assert_eq!(state.assistant.config().shared_prompt, "shared v2");
    }

    #[tokio::test]
    async fn reload_keeps_old_config_on_broken_file() {
        let dir = TempDir::new();
        let state = state_in(&dir);

        std::fs::write(state.config_path.clone(), "this is not = = toml {{{").unwrap();
        let reply = cmd(&state, r#"{"cmd":"reload"}"#);
        assert_eq!(reply["ok"], false, "{reply}");
        assert_eq!(state.assistant.config().shared_prompt, "shared v1");
    }

    #[tokio::test]
    async fn set_writes_and_applies() {
        let dir = TempDir::new();
        let state = state_in(&dir);

        let new_cfg = BASE_CONFIG.replace("shared v1", "shared v3");
        let reply = cmd(
            &state,
            &json!({"cmd": "set", "config": new_cfg}).to_string(),
        );
        assert_eq!(reply["ok"], true, "{reply}");
        assert_eq!(state.assistant.config().shared_prompt, "shared v3");
        let on_disk = std::fs::read_to_string(&state.config_path).unwrap();
        assert!(on_disk.contains("shared v3"), "{on_disk}");
    }

    #[tokio::test]
    async fn set_rolls_back_file_on_invalid_config() {
        let dir = TempDir::new();
        let state = state_in(&dir);

        let reply = cmd(&state, r#"{"cmd":"set","config":"broken = = ="}"#);
        assert_eq!(reply["ok"], false, "{reply}");
        assert_eq!(state.assistant.config().shared_prompt, "shared v1");
        let on_disk = std::fs::read_to_string(&state.config_path).unwrap();
        assert!(
            on_disk.contains("shared v1"),
            "file must be rolled back: {on_disk}"
        );
    }

    #[tokio::test]
    async fn set_rejects_runtime_bind_changes() {
        let dir = TempDir::new();
        let state = state_in(&dir);

        // Меняется только http_bind — конфиг валиден, но не применим.
        let new_cfg = BASE_CONFIG
            .replace("shared v1", "shared v4")
            .replace("0.0.0.0:11080", "127.0.0.1:9999");
        let reply = cmd(
            &state,
            &json!({"cmd": "set", "config": new_cfg}).to_string(),
        );
        assert_eq!(reply["ok"], false, "{reply}");
        assert!(
            reply["error"].as_str().unwrap().contains("http_bind"),
            "{reply}"
        );
        // Конфиг в памяти не тронут, файл откатан.
        assert_eq!(state.assistant.config().shared_prompt, "shared v1");
        let on_disk = std::fs::read_to_string(&state.config_path).unwrap();
        assert!(on_disk.contains("shared v1"), "{on_disk}");
    }

    #[tokio::test]
    async fn bind_uses_0600_and_replaces_stale_socket() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new();
        let state = state_in(&dir);

        let listener = bind(&state).expect("first bind");
        let mode = std::fs::metadata(&state.socket_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "socket must not be world-readable");

        // Слушатель закрыт, файл остался — следующий bind должен его заменить.
        drop(listener);
        let listener = bind(&state).expect("rebind over stale socket");
        drop(listener);
    }

    #[tokio::test]
    async fn serve_answers_over_the_socket() {
        let dir = TempDir::new();
        let state = state_in(&dir);
        let listener = bind(&state).expect("bind");
        tokio::spawn(serve(listener, state.clone()));

        let mut stream = UnixStream::connect(&state.socket_path)
            .await
            .expect("connect");
        stream
            .write_all(br#"{"cmd":"ping"}"#.as_slice())
            .await
            .expect("write cmd");
        stream.write_all(b"\n").await.expect("write newline");

        let mut reply = String::new();
        BufReader::new(&mut stream)
            .read_line(&mut reply)
            .await
            .expect("read");
        let parsed: Value = serde_json::from_str(reply.trim()).expect("json reply");
        assert_eq!(parsed["ok"], true, "{reply}");
        assert_eq!(parsed["service"], "obscape", "{reply}");
    }
}
