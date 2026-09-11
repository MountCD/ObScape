# ObScape
> The core to integrate AI into your project easily.
## Installation:
1. Clone this repo
2. Build project with `cargo build -r`
3. Run the http server with `cargo run -p ob_core` (Add ` -- --verbose` for verbose mode)
4. Make your config in opened text redactor or open file manually in `~/.config/obscape/config.toml`
5. Launch server again
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
