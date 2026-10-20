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
> Ensure what you have `docker` and `docker-compose` (docker-compose v2, not v1)
1. Clone this repo ```git clone https://github.com/mountcd/obscape```
2. Put ready to use `config.toml` in project catalog (leave `api_key` empty)
3. `cp .env.example .env` and fill in `POSTGRES_PASSWORD`, `OBSISTENT_API_TOKEN` and `OBSISTENT_AGENT_<NAME>_API_KEY`
4. Build project with `docker compose build`
5. Run project with `docker compose up`

> If `docker ps` answers `permission denied while trying to connect to the docker API`, your user is in the `docker` group but the current session has no token for it — log in again (or use `newgrp docker` / `sudo docker`).

Two access details worth knowing:
- **Model on the host**: inside the container `127.0.0.1` is the container itself. Compose maps `host.docker.internal` for you, so use `api_url = "http://host.docker.internal:1234"` — and make the model server listen on `0.0.0.0`, not only `127.0.0.1`, or the connection will be refused at the host.
- **Control socket**: it is created in the config directory (`/home/obscape/control.sock` in the container, `:rw` — only `config.toml` is mounted read-only). To reach it from the host, additionally mount a writable directory, e.g. `- "./.control:/home/obscape/.control"` with `control_socket = "/home/obscape/.control/control.sock"`.
## How to use it
If `OBSISTENT_API_TOKEN` (or `api_token` in `config.toml`) is set, every `/v1/chat/*` request must carry `Authorization: Bearer <token>`. `/v1/health` is always open and returns `503` when the database is unreachable.

> GET .../v1/health - get status of the core and its database

> POST .../v1/chat/new - make a new chat (`agent` is optional: defaults to the agent with `main = true`)
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

Both endpoints take two optional fields:

| field | meaning |
| --- | --- |
| `image` | a `data:image/<mime>;base64,...` URL or a bare base64 string (`png` / `jpeg` / `webp` / `gif`, up to 5 MiB). See [Vision](#vision-images). |
| `save_media` | `false` (default) keeps the image only in a `/tmp` cache; `true` also stores it in the database so the chat survives a cache wipe and a restart. |
| `stream` | `true` returns the answer as a Server-Sent Events stream instead of one JSON body. |

## Streaming
Add `?stream=true`, send `"stream": true` in the body, or set `Accept: text/event-stream`. The same request then answers with SSE: one event per generated chunk, then a final one that mirrors the plain response.

```bash
curl -N -H 'Content-Type: application/json' \
  -d '{"user_id":1,"message":"count to five"}' \
  'http://127.0.0.1:11080/v1/chat/new?stream=true'
```
```text
data: {"delta":"one"}

data: {"delta":" two"}

data: {"chat_id":3,"time":1792487719,"message":"one two ...","done":true}
```

An SSE keep-alive comment is sent every 15 s so proxies do not drop an idle stream. If the upstream fails *before* the stream starts, you get a normal JSON error with the matching status (`502` for LLM failures) — the stream only begins once the first bytes actually flow. If it breaks mid-stream, the stream emits `data: {"error":"..."}` and the user message is rolled back, exactly like the non-streaming path. Either way the assistant reply is written to the database **once, at the end**, not incrementally.

## Vision (images)
`image` accepts a data URL or bare base64. The core sniffs the format from the file signature (a `data:` MIME that contradicts the bytes is rejected), caches the file under `<tmp>/obscape-media/<sha256>` with `0600` rights and drops entries after 24 h. Then it sends the image to the model as an OpenAI `image_url` part (or as base64 for Ollama) on every request that includes it.

- **Temporary chat** (`save_media` false/omitted): the image lives only in that cache — nothing is written to the database.
- **Persistent chat** (`save_media: true`): the base64 is also stored in `messages.media`, so the picture can be replayed later.

`GET /v1/media/{message_id}` serves the image back (from the database first, then from the cache), protected by the same bearer token as the chat routes. `messages.media` is added automatically on startup (`ALTER TABLE ... ADD COLUMN IF NOT EXISTS`).

```bash
IMG=$(python3 -c 'import base64;print(base64.b64encode(open("cat.png","rb").read()).decode())')
curl -H 'Content-Type: application/json' \
  -d "{\"user_id\":1,\"message\":\"what is this?\",\"image\":\"$IMG\",\"save_media\":true}" \
  http://127.0.0.1:11080/v1/chat/new
```

## Backends
Each agent picks its backend with `backend` in its `config.toml` block:

```toml
# OpenAI-compatible API (default): api_url is the full chat-completions endpoint
backend = "openai"
api_url = "https://api.openai.com/v1/chat/completions"
api_key = "..."        # or OBSISTENT_AGENT_<NAME>_API_KEY in .env

# Local Ollama via ollama-rs: api_url is the server address, no key needed
backend = "ollama"
model_id = "llama3"
api_url = "http://127.0.0.1:11434"
api_key = ""
```

`timeout_secs` is per agent and applies to both backends.

## Live config reload (Unix socket)
The core keeps a Unix socket with `0600` rights — by default `~/.config/obscape/control.sock`, overridable with `control_socket` in `config.toml`. One JSON command per connection, one JSON line back:

```text
{"cmd":"ping"}                  -> {"ok":true,"service":"obscape"}
{"cmd":"get"}                   -> {"ok":true,"config":"<TOML, secrets as ***>"}
{"cmd":"reload"}                -> {"ok":true}   # re-read config.toml from disk
{"cmd":"set","config":"<TOML>"} -> {"ok":true}   # write the file and apply it
```

`set` and `reload` apply without a restart (handlers pick up the new snapshot on the next request), roll the file back if anything fails, and refuse changes to `database_url` / `http_bind` / `control_socket` — those need a restart. A `get` config echoes back masked secrets and is rejected as-is.

```bash
python3 -c "
import socket, os
s = socket.socket(socket.AF_UNIX)
s.connect(os.path.expanduser('~/.config/obscape/control.sock'))
s.sendall(b'{\"cmd\":\"reload\"}\n'); print(s.recv(65536).decode())"
```

## Python client
`python/` ships a small stdlib-only client for both the HTTP API and the control socket:

```bash
pip install ./python
```
```python
from obscape import ObScapeClient, ConfigControl, encode_image

client = ObScapeClient("http://127.0.0.1:11080", token="...")
print(client.health())
chat = client.new_chat(user_id=1, message="Hello")

# Streaming: yields {"delta": "..."} chunks, then {"chat_id":..., "done": True}
for event in client.stream_message(user_id=1, chat_id=chat["chat_id"], message="count to five"):
    print(event, end="", flush=True)

# Vision: a data URL, kept only for this chat (save_media=True stores it in the DB too)
client.new_chat(user_id=1, message="what is this?", image=encode_image("cat.png"), save_media=True)

control = ConfigControl()          # ~/.config/obscape/control.sock
control.reload()
print(control.get())               # current config, secrets masked
```

See `python/README.md` for details; `cd python && python3 -m unittest discover -v` runs its tests (no dependencies).
