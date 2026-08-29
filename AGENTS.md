# AGENTS.md

## Core Architecture
- **Language**: Rust
- **Runtime**: `tokio`
- **HTTP Server**: `axum`
- **Database**: Postgres (via `sqlx`)
- **Purpose**: An Obsidian assistant core that manages chat history in Postgres and interacts with LLMs.

## Key Components
- `src/main.rs`: Entry point, initializes config, DB, and starts the HTTP server.
- `src/server.rs`: HTTP handlers and routing.
- `src/database.rs`: Postgres interaction and message history management.
- `src/llm.rs`: LLM request logic and model handling.
- `src/config.rs`: Configuration loading and validation (TOML).

## API Endpoints
- `POST /v1/chat/new`: Create a new chat. Expects `NewChatIn` { `user_id`, `message`, `ai_type` }.
- `POST /v1/chat/messages`: Add message to existing chat. Expects `MessageIn` { `user_id`, `chat_id`, `message` }.
- `GET /v1/health`: Health check.

## Developer Commands
- Build: `cargo build`
- Run: `cargo run`
- Test: `cargo test`

## Important Notes
- **Configuration**: Uses `config.toml` by default. Supports CLI overrides for `--config` and `--vault`.
- **AI Types**: Assistant types are defined in `config.rs` (e.g., `talk`, `worker`, `audio`).
- **DB Schema**: Table creation is handled automatically on startup in `database::Database::open_db`.
