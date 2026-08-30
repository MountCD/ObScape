use serde::Deserialize;
use std::env;
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Deserialize, Clone)]
pub struct Prompt {
    pub system_prompt: String,
    pub personal_prompt: String,
}
impl Prompt {
    pub fn merge(&self) -> String {
        let mut new_prompt = String::new();
        new_prompt.push_str(&self.system_prompt);
        new_prompt.push_str(" ");
        new_prompt.push_str(&self.personal_prompt);
        new_prompt
    }
}
#[derive(Debug, Deserialize, Clone)]
pub struct TalkModel {
    pub enabled: bool,
    pub model_id: String,
    pub api_url: String,
    pub api_key: String,
}
#[derive(Debug, Deserialize, Clone)]
pub struct WorkerModel {
    pub enabled: bool,
    pub model_id: String,
    pub api_url: String,
    pub api_key: String,
}
#[derive(Debug, Deserialize, Clone)]
pub struct AudioModel {
    pub enabled: bool,
    pub model_id: String,
    pub api_url: String,
    pub api_key: String,
}
#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    pub database_url: String,
    /// Адрес и порт HTTP-сервера (например, `0.0.0.0:11080`).
    /// `None` означает "использовать дефолт".
    pub http_bind: Option<String>,
    pub prompt: Prompt,
    pub talk: TalkModel,
    pub worker: WorkerModel,
    pub audio: AudioModel,
}

/// Ошибки загрузки/парсинга конфигурации.
#[derive(Debug)]
pub enum ConfigError {
    Io(std::io::Error),
    Parse(toml::de::Error),
    MissingField(&'static str),
    InvalidArgs(String),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Io(e) => write!(f, "ошибка чтения конфига: {e}"),
            ConfigError::Parse(e) => write!(f, "ошибка разбора TOML: {e}"),
            ConfigError::MissingField(name) => write!(f, "в конфиге отсутствует поле `{name}`"),
            ConfigError::InvalidArgs(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for ConfigError {}

/// Дефолтный путь до файла конфигурации.
const DEFAULT_CONFIG_PATH: &str = "config.toml";

/// Дефолтный адрес HTTP-сервера.
const DEFAULT_HTTP_BIND: &str = "0.0.0.0:11080";

/// Переменные окружения, которые читаются как fallback.
const ENV_CONFIG: &str = "OBSISTENT_CONFIG";
const ENV_DATABASE: &str = "OBSISTENT_DATABASE";

/// Сырые переопределения путей, полученные из CLI / ENV.
#[derive(Debug, Default, Clone)]
struct PathOverrides {
    /// Путь до `config.toml` (CLI или ENV).
    config: Option<String>,
    /// Database URL (CLI или ENV).
    database: Option<String>,
    /// Запрошен ли `--print-config` (после `--print-config` нужно выйти).
    print_config: bool,
    /// Запрошен ли `--help` / `-h`.
    help: bool,
}

/// Распечатать краткую справку по аргументам командной строки.
pub fn print_help() {
    println!(
        "ObScape - ИИ ядро для вашего проекта.\n\n\
          Использование:\n  \
              obscape-server [ОПЦИИ]\n\n\
          Опции:\n  \
              --config <PATH>       Путь до config.toml (по умолчанию: {DEFAULT})\n  \
              --database <URL>      Database URL (перекрывает значение из config.toml)\n  \
              --print-config        Напечатать итоговый Config (для отладки) и выйти\n  \
              -h, --help            Показать эту справку и выйти\n\n\
          Переменные окружения:\n  \
              {ENV_CONFIG}            Аналог --config\n  \
              {ENV_DATABASE}          Аналог --database\n",
        DEFAULT = DEFAULT_CONFIG_PATH,
        ENV_CONFIG = ENV_CONFIG,
        ENV_DATABASE = ENV_DATABASE,
    );
}

/// Разобрать `argv` (без имени программы) в `PathOverrides`.
fn parse_args() -> Result<PathOverrides, ConfigError> {
    let mut out = PathOverrides::default();
    let mut it = env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-h" | "--help" => out.help = true,
            "--print-config" => out.print_config = true,
            "--config" => {
                out.config = Some(next_value(&mut it, "--config")?);
            }
            "--database" => {
                out.database = Some(next_value(&mut it, "--database")?);
            }
            other if other.starts_with("--config=") => {
                out.config = Some(strip_eq(other, "--config="));
            }
            other if other.starts_with("--database=") => {
                out.database = Some(strip_eq(other, "--database="));
            }
            other => {
                return Err(ConfigError::InvalidArgs(format!(
                    "неизвестный аргумент: `{other}` (попробуйте --help)"
                )));
            }
        }
    }

    // Если путь до конфига не задан через CLI — пробуем ENV.
    if out.config.is_none() {
        if let Ok(v) = env::var(ENV_CONFIG) {
            if !v.is_empty() {
                out.config = Some(v);
            }
        }
    }
    // То же для database.
    if out.database.is_none() {
        if let Ok(v) = env::var(ENV_DATABASE) {
            if !v.is_empty() {
                out.database = Some(v);
            }
        }
    }
    Ok(out)
}

fn next_value<I: Iterator<Item = String>>(
    it: &mut I,
    flag: &str,
) -> Result<String, ConfigError> {
    it.next()
        .ok_or_else(|| ConfigError::InvalidArgs(format!("флаг `{flag}` требует значение")))
}

fn strip_eq(arg: &str, prefix: &str) -> String {
    arg[prefix.len()..].to_string()
}

/// Загрузить конфиг с учётом CLI-аргументов и ENV.
///
/// Приоритет: CLI > ENV > значение из config.toml.
pub fn load_config_with_args() -> Result<Config, ConfigError> {
    let overrides = parse_args()?;

    if overrides.help {
        print_help();
        // Не паникуем и не возвращаем config — выходим с кодом 0.
        std::process::exit(0);
    }

    // 1. Определяем путь до config.toml.
    let config_path = overrides
        .config
        .clone()
        .unwrap_or_else(|| DEFAULT_CONFIG_PATH.to_string());
    let config_path = PathBuf::from(&config_path);

    // 2. Читаем и парсим TOML.
    let contents = fs::read_to_string(&config_path).map_err(ConfigError::Io)?;
    let mut config: Config = toml::from_str(&contents).map_err(ConfigError::Parse)?;

    // 3. Применяем override для database (CLI > ENV > config.toml).
    if let Some(database) = overrides.database {
        config.database_url = database;
    }

    // 4. Пост-валидация критичных полей.
    if config.database_url.trim().is_empty() {
        return Err(ConfigError::MissingField("database_url"));
    }
    // 5. Дефолт для http_bind, если в config.toml не задан.
    if config
        .http_bind
        .as_deref()
        .map(str::trim)
        .map(str::is_empty)
        .unwrap_or(true)
    {
        config.http_bind = Some(DEFAULT_HTTP_BIND.to_string());
    }
    if !config_path.exists() {
        // Не критично для самого парсинга, но сообщим сразу — файл мог быть
        // создан в OBSISTENT_CONFIG, но не существовать на диске.
        eprintln!(
            "предупреждение: файл конфига `{}` не найден",
            config_path.display()
        );
    }

    if overrides.print_config {
        print_config(&config);
        std::process::exit(0);
    }

    Ok(config)
}

/// Pretty-print конфигурации для `--print-config`.
pub fn print_config(config: &Config) {
    println!("database_url  = {}", config.database_url);
    println!(
        "http_bind     = {}",
        config.http_bind.as_deref().unwrap_or(DEFAULT_HTTP_BIND)
    );
    println!("prompt.system = {}", config.prompt.system_prompt);
    println!("prompt.person = {}", config.prompt.personal_prompt);
    println!(
        "talk   = {{ enabled: {}, model: {}, url: {} }}",
        config.talk.enabled, config.talk.model_id, config.talk.api_url
    );
    println!(
        "worker = {{ enabled: {}, model: {}, url: {} }}",
        config.worker.enabled, config.worker.model_id, config.worker.api_url
    );
    println!(
        "audio  = {{ enabled: {}, model: {}, url: {} }}",
        config.audio.enabled, config.audio.model_id, config.audio.api_url
    );
}

/// Удобный пресет: загрузить конфиг из `config.toml` без CLI-аргументов.
///
/// Сохранён для обратной совместимости с прежним API.
pub fn load_config() -> Config {
    load_config_with_args().expect("не удалось загрузить конфиг")
}

/// Удобство для `main()`: вернуть код возврата в зависимости от того,
/// попросил ли пользователь `--help` / `--print-config`.
/// Возвращаем `i32`, чтобы `std::process::exit` мог принять значение
/// и на stable Rust (где `ExitCode::process` недоступен).
pub fn dispatch_cli() -> Result<Config, i32> {
    match load_config_with_args() {
        Ok(cfg) => Ok(cfg),
        Err(e) => {
            eprintln!("{e}");
            Err(2)
        }
    }
}
