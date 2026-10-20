# HTTP-клиент `ObScapeClient`

Модуль `obscape.client`. Синхронный клиент REST API ядра поверх
`urllib.request` — без сторонних зависимостей.

## Конструктор

```python
ObScapeClient(
    base_url: str = "http://127.0.0.1:11080",
    token: Optional[str] = None,
    timeout: float = 180.0,
)
```

| Параметр | Значение |
| --- | --- |
| `base_url` | Адрес ядра; конечный `/` отрезается (`rstrip("/")`). |
| `token` | Bearer-токен (`api_token` из конфига). Сервер без токена → оставляйте `None`; заголовок `Authorization` не шлётся. |
| `timeout` | Таймаут одного запроса в секундах. Дефолт 180 с — как у серверного request-timeout; ответ модели может идти долго. |

Клиент без состояния: каждый метод — ровно один HTTP-запрос, переиспользуйте
один экземпляр.

## Методы

### `health() -> dict`

`GET /v1/health`. Токен не требуется.

Ответ `200` → `{"status": "ok", "db": "ok"}`. Ответ `503`
(`{"status": "degraded", "db": ...}`) — **не ошибка**: payload возвращается
как словарь. Любой другой HTTP >= 400 → `ApiError`.

### `new_chat(user_id: int, message: str, agent: Optional[str] = None) -> dict`

`POST /v1/chat/new` — новый чат с первым сообщением пользователя.

- `agent` — ключ из `config.agents`; не указан → агент с `main = true`
  (в поле JSON не попадает вообще).
- Возвращает `{chat_id, time, message, tools}`: `time` — Unix-секунды,
  `tools` сейчас всегда `[]`.

### `send_message(user_id: int, chat_id: int, message: str) -> dict`

`POST /v1/chat/message` — сообщение в существующий чат. Форма ответа та же,
что у `new_chat`. Чат, принадлежащий другому `user_id`, неотличим от
несуществующего — сервер ответит 400 `unknown chat_id`.

## Ошибки

| Исключение | Когда | Поля |
| --- | --- | --- |
| `ApiError` | Сервер ответил HTTP >= 400 | `status`, `message`, `payload` (разобранный JSON-объект или `None`) |
| `ObScapeError` | Сервер недоступен (`URLError`/`OSError`) либо тело ответа не JSON-объект | — |

`ApiError` — подкласс `ObScapeError`, так что `except ObScapeError` ловит и то,
и другое. `message` берётся из поля `{"error": ...}` ответа, иначе — сырой
текст тела. Контекст исходного `HTTPError` не сохраняется (`from None`) —
всё, что нужно, лежит в полях `ApiError`.

```python
from obscape import ApiError, ObScapeError, ObScapeClient

client = ObScapeClient(token="...")
try:
    client.new_chat(user_id=1, message="")
except ApiError as e:
    print(e.status, e.message, e.payload)   # 400 empty message {...}
except ObScapeError as e:
    print("cannot reach server:", e)
```
