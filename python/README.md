# obscape

Python-клиент [ObScape](https://github.com/mountcd/obscape): HTTP API чата и
Unix-сокет управления конфигом. Только стандартная библиотека — внешних
зависимостей нет.

```bash
pip install ./python
```

## HTTP API

```python
from obscape import ObScapeClient

client = ObScapeClient("http://127.0.0.1:11080", token="...")  # token — из api_token
print(client.health())                       # {"status": "ok", "db": ...}

chat = client.new_chat(user_id=1, message="Привет!")   # {"chat_id": ..., ...}
reply = client.send_message(user_id=1, chat_id=chat["chat_id"], message="А теперь?")
```

Ошибки сервера поднимаются как `ApiError` (с полями `status`, `message`,
`payload`), недоступность адреса — как `ObScapeError`.

## Управление конфигом через Unix-сокет

Ядро держит сокет `<каталог конфига>/control.sock` (путь меняется полем
`control_socket` в `config.toml`). Протокол — одна JSON-команда на соединение.

```python
from obscape import ConfigControl

control = ConfigControl()                    # ~/.config/obscape/control.sock
control.ping()                               # -> {"ok": True, "service": "obscape"}

toml_text = control.get()                    # текущий конфиг, секреты = "***"
print(control.get_dict()["history_limit"])   # то же, разобранное (нужен tomllib)

control.reload()                             # перечитать config.toml с диска
control.set(open("new_config.toml").read())  # записать файл и применить
control.set_from_file("new_config.toml")     # то же из файла
```

`get` отдаёт маскированный конфиг — отправлять его обратно в `set` сервер
отклоняет. `set` и `reload` откатывают файл при любой ошибке, а изменения
`database_url` / `http_bind` / `control_socket` в рантайме запрещены — нужен
рестарт. Ошибки сокета и отказы сервера поднимаются как `ControlError`.

Путь к сокету задаётся явно: `ConfigControl("/run/obscape/control.sock")`
либо переменной окружения `OBSISTENT_CONFIG` (каталог конфига, тот же,
который читает `ob_core`).

## Тесты

```bash
cd python && python -m unittest discover -v
```
