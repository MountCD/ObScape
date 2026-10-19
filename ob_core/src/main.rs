use ob_common::{
    config::{self, format_conf_path},
    database,
};
use ob_lib::{Assistant, ObScapeError};
pub mod control;
pub mod server;

#[tokio::main]
async fn main() {
    // .env из текущей директории (если есть). Уже заданные переменные
    // окружения имеют приоритет — dotenvy их не перезаписывает.
    let _ = dotenvy::dotenv();

    // 0. Разбираем аргументы и ENV один раз.
    let overrides = match config::parse_cli() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(2);
        }
    };

    // `--help` / `--init` обрабатываем до того, как трогаем конфиг:
    // справка не должна ничего создавать на диске.
    if overrides.help() {
        config::print_help();
        return;
    }
    if overrides.init() {
        if let Err(e) = config::init_config(&overrides) {
            eprintln!("error: {e}");
            std::process::exit(2);
        }
        return;
    }

    // Авто-инициализация, если конфиг не найден.
    let conf_path = match config::resolve_config_path(&overrides) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(2);
        }
    };
    let path = format_conf_path(conf_path.clone());
    if !std::path::Path::new(&path).exists() {
        if config::is_containerized() {
            eprintln!(
                "Config file not found at {path}. Mount a valid config.toml into the container."
            );
            std::process::exit(2);
        }
        eprintln!("Config file is not found. Creating new...");
        match config::init_config(&overrides) {
            Ok(()) => eprintln!("Config template created. Edit it and start again."),
            Err(error) => {
                eprintln!("error: {error}");
                std::process::exit(2);
            }
        }
    }

    // 1. Проверяем итоговый конфиг на ошибки.
    let cfg = match config::dispatch_cli(&overrides) {
        Ok(c) => c,
        Err(code) => std::process::exit(code),
    };
    if overrides.print_config() {
        config::print_config(&cfg);
        return;
    }

    // Логи в stderr всегда: ошибки и 5xx — на info, в verbose — каждый
    // запрос (tower_http=debug). RUST_LOG переопределяет фильтр целиком.
    let default_filter = if cfg.verbose {
        "info,tower_http=debug"
    } else {
        "info"
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| default_filter.into()),
        )
        .with_writer(std::io::stderr)
        .init();

    // 2. Открываем БД (создаём таблицу при первом запуске).
    ob_common::vlog!(
        &cfg,
        "connecting to database: {}",
        config::mask_db_password(&cfg.database_url)
    );
    let db = match database::Database::open_db(&cfg.database_url).await {
        Ok(d) => d,
        Err(e) => {
            eprintln!("failed to connect to Postgres: {e}");
            std::process::exit(3);
        }
    };

    // 3-4. Запускаем ядро на порту и слушаем запросы.
    let bind_addr = cfg
        .http_bind
        .clone()
        .unwrap_or_else(|| config::DEFAULT_HTTP_BIND.to_string());
    if cfg.api_token.is_none() && !is_loopback(&bind_addr) {
        tracing::warn!(
            "api_token is not set and the server listens on {bind_addr}: \
             /v1/chat/* is open to anyone who can reach it (set {})",
            config::ENV_API_TOKEN
        );
    }

    // Путь к Unix-сокету управления: явный `control_socket` из конфига
    // или `<каталог конфига>/control.sock` по умолчанию.
    let control_path = std::path::PathBuf::from(
        cfg.control_socket
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("{conf_path}/control.sock")),
    );

    let state = server::AppState::new(db, cfg);
    let control_state = control::ControlState {
        assistant: state.assistant.clone(),
        overrides: overrides.clone(),
        config_path: std::path::PathBuf::from(path.clone()),
        socket_path: control_path.clone(),
        http_bind: bind_addr.clone(),
    };
    // Сокет биндим до старта HTTP: ошибку вида «занято» нужно увидеть сразу.
    let control_listener = match control::bind(&control_state) {
        Ok(l) => l,
        Err(e) => {
            eprintln!(
                "failed to bind control socket {}: {e}",
                control_path.display()
            );
            std::process::exit(6);
        }
    };
    tokio::spawn(control::serve(control_listener, control_state));

    println!("obscape: listening on http://{bind_addr}");
    println!("  POST /v1/chat/new      - new chat with the first message");
    println!("  POST /v1/chat/message  - message in an existing chat");
    println!("  GET  /v1/health        - health check");
    println!(
        "  control socket         - get / reload / set ({})",
        control_path.display()
    );

    let app = server::router(state);
    let listener = match tokio::net::TcpListener::bind(&bind_addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("failed to bind {bind_addr}: {e}");
            std::process::exit(4);
        }
    };

    let server_result = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await;

    // Убираем за собой сокет, чтобы следующий запуск не спотыкался
    // о файл от остановленного процесса.
    let _ = std::fs::remove_file(&control_path);

    if let Err(e) = server_result {
        eprintln!("server exited with an error: {e}");
        std::process::exit(5);
    }
}

/// Ждёт SIGINT (Ctrl+C) или SIGTERM (docker stop) для мягкой остановки.
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install SIGINT handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }

    eprintln!("shutdown signal received, draining connections...");
}

/// `127.0.0.1:*` / `localhost:*` / `[::1]:*` — адрес, на который снаружи
/// не попасть; для него отсутствие `api_token` не считается проблемой.
fn is_loopback(bind: &str) -> bool {
    bind.parse::<std::net::SocketAddr>()
        .map(|a| a.ip().is_loopback())
        .unwrap_or_else(|_| bind.starts_with("localhost:"))
}
