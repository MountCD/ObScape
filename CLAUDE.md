# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

```bash
cargo build                      # build workspace
cargo run -p ob_core             # run the HTTP server binary
cargo run -p ob_core -- --help   # CLI flags (--config, --database, --init, --verbose)
cargo test                       # run all tests (currently only unit tests in ob_common/src/time.rs)
cargo test -p ob_common <name>   # run a single test by name in one crate
```

CI (`.github/workflows/rust.yml`) only runs `cargo build --verbose` and `cargo test --verbose` on push/PR to `master`. No lint/fmt step, so `cargo fmt` / `cargo clippy` drift is not caught automatically — run them locally before pushing.

Runtime requires a reachable PostgreSQL instance (`database_url` in config). There is no migration tool — `Database::open_db` issues `CREATE TABLE IF NOT EXISTS` for `messages` and `chats` plus an index on every connect.

Docker: `docker compose build && docker compose up` brings up `postgres:16` plus the server; `docker-compose.yml` bind-mounts `./config.toml` (gitignored, must exist first) to `/etc/obscape/config.toml`. The multi-stage `Dockerfile` uses `cargo-chef` for dependency caching and builds only `-p ob_core`.

## Architecture

Cargo workspace (edition 2024, resolver 3) with three crates layered bottom-up:

- **`ob_common`** — shared primitives, no knowledge of HTTP.
  - `config.rs`: `Config` / `AgentConfig`, TOML loading, hand-rolled argv parsing, `--init` template generation.
  - `database.rs`: `Database` (thin `PgPool` wrapper) and the wire types. `JsonMessageContent { role, content: ContentStruc }` is the DB row shape; `LlmMessage { role, content: String }` (built via `From<&JsonMessageContent>`) is what actually goes upstream inside `JsonRequestMessage`.
  - `agent.rs`: `make_request_with` — the only place an upstream LLM is called (OpenAI-compatible chat completions, reads `choices[0].message.content`, bearer auth). `resolve_agent` looks up `config.agents[key]` and rejects `enabled = false`. `make_request()` is a legacy wrapper that opens config+DB itself and exports *every* chat; do not use it from the server path.
  - `time.rs`: dependency-free ISO-8601 UTC timestamps. Message `time` is stored as TEXT and ordered lexicographically in SQL, so keep the `YYYY-MM-DDTHH:MM:SS.mmmZ` format.
  - `verbose.rs`: `vlog!(&cfg, ...)` and `vdbg!(&cfg, expr)` macros — print to stderr with a `[verbose] file:line]` prefix only when `cfg.verbose`. `vdbg!` returns the value like `dbg!`. Use these instead of ad-hoc `if cfg.verbose`.
- **`ob_lib`** — `Assistant`, the orchestration layer. Errors funnel into `ObScapeError::{Db, Llm, BadRequest, Internal}`.
  - `create_chat`: resolve agent → `INSERT INTO chats ... RETURNING chat_id` → write the system prompt (`shared_prompt + personality_prompt`) as a `System` message once → delegate to `send_message`.
  - `send_message`: look up the chat's agent from the `chats` table (agent is fixed per chat; unknown `chat_id` → `BadRequest`) → save user message → `export_chat` history → `make_request_with` → save assistant reply.
- **`ob_core`** — the binary. `main.rs` auto-generates a config if missing, dispatches CLI, opens the DB, then serves the Axum router with a SIGINT/SIGTERM graceful shutdown. `server.rs` holds `AppState { assistant, cfg }`, DTOs, and `AppError` → HTTP status mapping (`Db`/`Internal` → 500, `Llm` → 502, `BadRequest` → 400).

New behaviour belongs in `Assistant` (`ob_lib`) so the DB/LLM sequencing is not duplicated; handlers in `ob_core/src/server.rs` should stay thin validation + DTO shims.

Note: `ob_core/src/server.rs` refers to `crate::Assistant`, `crate::config`, `crate::database`, `crate::ObScapeError` — these resolve through the `use` statements at the top of `main.rs`, not through a `lib.rs`. Adding an import there changes what `server.rs` can see.

## Endpoints

- `GET  /v1/health`
- `POST /v1/chat/new` — `{ user_id, message, agent }` → `{ chat_id, time, message, tools }`
- `POST /v1/chat/message` — same response shape, takes `{ user_id, chat_id, message }`

`agent` is a key in `config.agents` (a `HashMap<String, AgentConfig>`), not a fixed enum — agents are defined entirely in TOML and are chosen only at chat creation. `time` in responses is Unix seconds; `tools` is always empty for now.

## Config

`--config` takes the **directory** holding `config.toml`, not the file path: `resolve_config_path` returns the directory and `format_conf_path` appends `/config.toml`. The default directory is `$HOME/.config/obscape` via `env::home_dir()`.

Precedence: CLI > env > TOML. The env vars actually read by `parse_args` are `OBSISTENT_CONFIG` (config directory) and `OBSISTENT_DATABASE` (database URL) — note that `Dockerfile` sets `OBSISTENT_CONFIG_DIR` and `docker-compose.yml` sets `OBSISTENT_CONFIG_PATH`, neither of which the code reads, so containers currently fall back to the default path. `config.toml` and `.config/` are gitignored.

The template written by `--init` sets `autogenerated = true`; `load_config` refuses to start while that flag is set (opens the file via `xdg-open`/`notepad` and returns `ConfigError::Autogenerated`), forcing the user to edit and clear it. `--verbose` (or `verbose = true`) bypasses this check as a side effect.

`is_containerized()` gates every attempt to spawn an editor (checks `OBSISTENT_NO_EDITOR`, `/.dockerenv`, `$container`, `/proc/1/cgroup`) — keep new editor-spawning paths behind it.

`--print-config` appears in `--help` but is not parsed by `parse_args`, so it is rejected as an unknown argument — and because `main.rs` calls `resolve_config_path().unwrap()`, a bad argument panics instead of printing the error.

## Conventions

Doc comments and user-facing CLI/log strings are largely in Russian; match the surrounding file rather than translating. Verbose output goes through the `vlog!`/`vdbg!` macros (stderr), not `println!` — though a few stray `dbg!` calls remain in `config.rs`. `AGENTS.md` just points here.

`CODE_REVIEW.md` lists known bugs and rough edges with file:line references — check it before assuming surprising behaviour is intentional.
