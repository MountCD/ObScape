use ob_common::{config, database};
use ob_lib::{Assistant, ObScapeError};
use tokio;
pub mod server;

#[tokio::main]
async fn main() {
    // 0. Авто-инициализация, если конфиг не найден.
    if !std::path::Path::new(&config::make_file_path()).exists() {
        eprintln!("Config file is not found. Creating new...");
        match config::init_config() {
            Ok(()) => {
                eprintln!("Config template created. Edit it and start again.");
            }
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

    if let Err(e) = axum::serve(listener, app).await {
        eprintln!("server exited with an error: {e}");
        std::process::exit(5);
    }
}
