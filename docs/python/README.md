# obscape — Python-клиент

Документация пакета `python/obscape` — клиента ObScape на Python: HTTP API чата
и Unix-сокет управления конфигом. Только стандартная библиотека, внешних
зависимостей нет (Python >= 3.9; `get_dict()` требует 3.11+ из-за `tomllib`).

```bash
pip install ./python
```

Публичный экспорт (`obscape/__init__.py`): `ObScapeClient`, `ConfigControl`,
`ApiError`, `ControlError`, `ObScapeError`, `default_socket_path`.

## Быстрый старт

```python
from obscape import ObScapeClient, ConfigControl

client = ObScapeClient("http://127.0.0.1:11080", token="...")
print(client.health())                              # {"status": "ok", "db": "ok"}
chat = client.new_chat(user_id=1, message="Привет!")
client.send_message(user_id=1, chat_id=chat["chat_id"], message="А теперь?")

control = ConfigControl()                           # ~/.config/obscape/control.sock
control.ping()                                      # {"ok": True, "service": "obscape"}
control.reload()                                    # перечитать config.toml на сервере
```

## Содержание

- [HTTP-клиент `ObScapeClient`](client.md) — `health`, `new_chat`, `send_message`,
  ошибки `ApiError` / `ObScapeError`.
- [Сокет управления `ConfigControl`](control.md) — `ping`, `get`, `get_dict`,
  `reload`, `set`, `set_from_file`, путь к сокету, ошибки `ControlError`.
- [Синхронизация с ядром](protocol.md) — протоколы и точки, которые обязаны
  совпадать с Rust-стороной.

## Тесты

```bash
cd python && python3 -m unittest discover -v
```

Тесты без внешних зависимостей и без запущенного ядра: `tests/test_client.py`
поднимает fake-HTTP-сервер на `http.server`, `tests/test_control.py` — fake
Unix-сокет.

## Правила обновления документации

При любом изменении `python/obscape/`, `python/pyproject.toml`, `python/README.md`
или протоколов, которым соответствует клиент, **обновляйте файлы в `docs/python/`
в том же коммите** — см. раздел «Документация» в `CLAUDE.md`.
