use ob_common::{config, database};
use ob_lib::{Assistant, ObScapeError};
pub mod server;

#[tokio::main]
async fn main() {
    // 0. Авто-инициализация, если конфиг не найден.
    let conf_path = config::resolve_config_path().unwrap(); // или обработать ошибку так же, как раньше
    if !std::path::Path::new(&conf_path).exists() {
        if config::is_containerized() {
            eprintln!(
                "Config file not found at {conf_path}. Mount a valid config.toml into the container."
            );
            std::process::exit(2);
        }
        eprintln!("Config file is not found. Creating new...");
        match config::init_config() {
            Ok(()) => eprintln!("Config template created. Edit it and start again."),
            Err(error) => {
                eprintln!("error: {error}");
                return;
            }
        }
    }

    // 1. Проверяем итоговый конфиг на ошибки.
    let cfg = match config::dispatch_cli() {
        Ok(c) => c,
        Err(code) => std::process::exit(code),
    };

    // 2. Открываем БД (создаём таблицу при первом запуске).
    ob_common::vlog!(&cfg, "connecting to database: {}", cfg.database_url);
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
        .unwrap_or_else(|| "0.0.0.0:11080".to_string());
    let state = server::AppState::new(db, cfg);

    println!("obscape: listening on http://{bind_addr}");
    println!("  POST /v1/chat/new      - new chat with the first message");
    println!("  POST /v1/chat/message  - message in an existing chat");
    println!("  GET  /v1/health        - health check");

    let app = server::router(state);
    let listener = match tokio::net::TcpListener::bind(&bind_addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("failed to bind {bind_addr}: {e}");
            std::process::exit(4);
        }
    };

    // main.rs, вместо axum::serve(listener, app).await
    if let Err(e) = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
    {
        eprintln!("server exited with an error: {e}");
        std::process::exit(5);
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
}
