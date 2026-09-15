# ObScape
> The core to integrate AI into your project easily.
## Installation:
### Local:
1. Clone this repo ```git clone https://github.com/mountcd/obscape```
2. Build project with `cargo build -r`
3. Run the http server with `cargo run -p ob_core` (Add ` -- --verbose` for verbose mode)
4. Make your config in opened text redactor or open file manually in `~/.config/obscape/config.toml`
5. Put secrets into `.env` in the working directory (see `.env.example`): `OBSISTENT_AGENT_<NAME>_API_KEY` for each agent, optionally `OBSISTENT_DATABASE`. They override `api_key` / `database_url` from `config.toml`
6. Launch server again
### Docker:
> Ensure what you have `docker` and `docker-compose`
1. Clone this repo ```git clone https://github.com/mountcd/obscape```
2. Put ready to use `config.toml` in project catalog (leave `api_key` empty)
3. `cp .env.example .env` and fill in `POSTGRES_PASSWORD` and `OBSISTENT_AGENT_<NAME>_API_KEY`
4. Build project with `docker compose build`
5. Run project with `docker compose up`
## How to use it
> GET .../v1/health - get status of the core,

> POST .../v1/chat/new - make a new chat
```json
{
  "user_id": 0,
  "message": "Hello",
  "agent": "primary"
}
```

> POST .../v1/chat/message - send a message to existing chat
```json
{
  "user_id": 0,
  "chat_id": 3,
  "message": "How are you?"
}
```
