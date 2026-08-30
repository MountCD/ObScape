use ob_lib::ob_common::{config, database, llm};
use ob_lib::{Assistant, ObScapeError};
use tokio;
pub mod server;

#[tokio::main]
async fn main() {
    // 1. Проверяем итоговый конфиг на ошибки.
    let cfg = match config::dispatch_cli() {
        Ok(c) => c,
        Err(code) => std::process::exit(code),
    };

    // 2. Открываем БД (создаём таблицу при первом запуске).
    let db = match database::Database::open_db(&cfg.database_url).await {
        Ok(d) => d,
        Err(e) => {
            eprintln!("не удалось подключиться к Postgres: {e}");
            std::process::exit(3);
        }
    };

    // 3-4. Запускаем ядро на порту и слушаем запросы.
    let bind_addr = cfg
        .http_bind
        .clone()
        .unwrap_or_else(|| "0.0.0.0:11080".to_string());
    let state = server::AppState::new(db, cfg);

    println!("obsistent: слушаю на http://{bind_addr}");
    println!("  POST /v1/chat/new       — новый чат с первым сообщением");
    println!("  POST /v1/chat/messages  — сообщение в существующий чат");
    println!("  GET  /v1/health         — проверка работоспособности");

    let app = server::router(state);
    let listener = match tokio::net::TcpListener::bind(&bind_addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("не удалось занять {bind_addr}: {e}");
            std::process::exit(4);
        }
    };

    if let Err(e) = axum::serve(listener, app).await {
        eprintln!("сервер завершился с ошибкой: {e}");
        std::process::exit(5);
    }
}
