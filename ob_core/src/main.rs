use ob_common::{
    config::{self, format_conf_path},
    database,
};
use tokio::os::unix::net::{UnixStream, UnixListener};

#[tokio::main]
async fn main() {
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
