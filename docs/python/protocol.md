# Синхронизация клиента с ядром

Пакет `python/obscape` — обёртка над двумя протоколами ядра. При их изменении
Rust-сторона и клиент (и эта папка) должны обновляться вместе.

## HTTP API (`obscape.client` ↔ `ob_core/src/server.rs`)

| Клиент | Эндпоинт | Аутентификация |
| --- | --- | --- |
| `health()` | `GET /v1/health` | нет (единственный эндпоинт без токена) |
| `new_chat(...)` | `POST /v1/chat/new` | `Authorization: Bearer <api_token>`, если задан |
| `send_message(...)` | `POST /v1/chat/message` | то же |

Совпадать должны:

- форма запросов: `new_chat` → `{user_id, message, agent?}`, `send_message` →
  `{user_id, chat_id, message}`;
- форма ответов: `{chat_id, time, message, tools}`, `time` — Unix-секунды,
  `tools` — массив (сейчас всегда пустой);
- коды ошибок: 400 (валидация/`BadRequest`), 401 (неверный токен), 413
  (тело > 64 КиБ), 408 (таймаут обработки), 500, 502 (апстрим), 503
  (`health`, `degraded`);
- тело ошибки — JSON с полем `error`; клиент разбирает именно его;
- дефолтный таймаут клиента 180 с равен серверному request-timeout.

Клиент шлёт токен при каждом запросе: сервер читает `api_token` из актуального
конфига, так что `set`/`reload` по сокету меняют и проверку токена без
рестарта.

## Контрольный сокет (`obscape.control` ↔ `ob_core/src/control.rs`)

| Команда клиента | Протокол | Ответ |
| --- | --- | --- |
| `ping()` | `{"cmd":"ping"}` | `{"ok":true,"service":"obscape"}` |
| `get()` | `{"cmd":"get"}` | `{"ok":true,"config":"<TOML с масками>"}` |
| `reload()` | `{"cmd":"reload"}` | `{"ok":true}` |
| `set(text)` | `{"cmd":"set","config":"<TOML>"}` | `{"ok":true}` |
| неизвестная команда | — | `{"ok":false,"error":"..."}` → `ControlError` |

Совпадать должны:

- построчный формат: одна строка JSON на соединение в каждую сторону
  (клиент дописывает `\n` и читает до первой `\n`);
- порядок ≤ 1 МиБ на команду (ядро) и ≤ 4 МиБ на ответ (клиент,
  `MAX_REPLY_BYTES`);
- путь сокета: `OBSISTENT_CONFIG` → иначе `$HOME/.config/obscape`, плюс
  `control.sock` — одинаковая логика `default_socket_path()` в клиенте и
  выбора пути в `main.rs`;
- правила отказов: запрещённые в рантайме поля (`database_url`, `http_bind`,
  `control_socket`) и маски `***` в тексте → `{"ok":false,...}`, откат файла
  при ошибке `set`.

## Версии

`__version__` в `python/obscape/__init__.py` и `version` в
`python/pyproject.toml` держатся одинаковыми; при выпуске пакета меняются
вместе.
