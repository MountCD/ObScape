# AGENTS.md

## Core Architecture
- **Language**: Rust (Tokio, Axum, SQLx)
- **Structure**: Cargo Workspace
    - `ob_common`: Shared utilities (config, database, llm).
    - `ob_lib`: Core logic; provides `Assistant` for chat/LLM orchestration.
    - `ob_core`: HTTP server implementation (binary).
- **Database**: Postgres (automatic table creation via `Database::open_db`).

## Key Components
- `ob_core/src/main.rs`: Server entry point; handles CLI config and Axum startup.
- `ob_core/src/server.rs`: HTTP routing and DTOs.
- `ob_lib/src/lib.rs`: Main `Assistant` struct for high-level API.
- `ob_common/src/config.rs`: TOML config loading with CLI/ENV overrides.
- `ob_common/src/llm.rs`: LLM request logic.

## API Endpoints
- `POST /v1/chat/new`: Create chat + first message. Body: `{ user_id, message, ai_type }`.
- `POST /v1/chat/messages`: Add message to chat. Body: `{ user_id, chat_id, message }`.
- `GET /v1/health`: Health check.

## Developer Commands
- Build: `cargo build`
- Run Server: `cargo run -p ob_core`
- Test: `cargo test`

## Important Notes
- **Config**: Default is `config.toml`. Supports `--config <PATH>` and `--database <PATH>` CLI flags.
- **AI Types**: Model selection is defined in `Config` (talk, worker, audio).
- **Convention**: Use `Assistant` in `ob_lib` for new logic to avoid duplicating DB/LLM orchestration.
