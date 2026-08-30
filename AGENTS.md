# AGENTS.md

## Core Architecture
- **Project Name**: ObScape
- **Language**: Rust (Tokio, Axum, SQLx)
- **Structure**: Hybrid Library/Binary
    - `obscape_core` (lib): High-level API via `Assistant` struct for chat management and LLM interaction.
    - `obscape_server` (bin): HTTP wrapper around the core library.
- **Database**: Postgres (automatic table creation on startup via `Database::open_db`).

## Key Components
- `src/lib.rs`: Entry point for the library; contains `Assistant` for simplified access to LLM/DB.
- `src/main.rs`: Server entry point; handles CLI config and starts the Axum server.
- `src/server.rs`: HTTP routing and DTOs.
- `src/llm.rs`: LLM request logic.
- `src/config.rs`: TOML config loading with CLI/ENV overrides.

## API Endpoints
- `POST /v1/chat/new`: Create new chat + first message. Expects `{ user_id, message, ai_type }`.
- `POST /v1/chat/message`: Add message to existing chat. Expects `{ user_id, chat_id, message }`.
- `GET /v1/health`: Health check.

## Developer Commands
- Build: `cargo build`
- Run Server: `cargo run --bin obscape_server`
- Test: `cargo test`

## Important Notes
- **Config**: Default is `config.toml`. Supports `--config <PATH>` and `--vault <PATH>` CLI flags.
- **AI Types**: Model selection is defined in `Config` (talk, worker, audio).
- **Convention**: Use `Assistant` struct for any new logic to avoid duplicating DB/LLM orchestration.
