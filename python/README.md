# obscape

Python-клиент [ObScape](https://github.com/mountcd/obscape): HTTP API чата и
Unix-сокет управления конфигом. Только стандартная библиотека — внешних
зависимостей нет.

```bash
pip install ./python
```

## HTTP API

```python
from obscape import ObScapeClient, encode_image

client = ObScapeClient("http://127.0.0.1:11080", token="...")  # token — из api_token
print(client.health())                       # {"status": "ok", "db": ...}

chat = client.new_chat(user_id=1, message="Привет!")   # {"chat_id": ..., ...}
reply = client.send_message(user_id=1, chat_id=chat["chat_id"], message="А теперь?")
```

Ошибки сервера поднимаются как `ApiError` (с полями `status`, `message`,
`payload`), недоступность адреса — как `ObScapeError`.

### Стриминг

`stream_new_chat` / `stream_message` возвращают генератор по SSE. Он отдаёт
словари по мере генерации: `{"delta": "..."}` для порций текста и финальный
`{"chat_id": ..., "time": ..., "message": ..., "done": True}`; при обрыве
модели — `{"error": "..."}` и конец генератора.

```python
for event in client.stream_new_chat(user_id=1, message="Посчитай до пяти"):
    if "delta" in event:
        print(event["delta"], end="", flush=True)
    else:
        print("\nфинал:", event)          # {"chat_id": ..., "done": True, ...}

# то же для существующего чата
for event in client.stream_message(user_id=1, chat_id=chat["chat_id"], message="Ещё"):
    ...
```

### Картинки (vision)

`image` принимает data-URL или голый base64 (`encode_image` делает data-URL
из файла). `save_media=True` дополнительно сохраняет картинку в БД, иначе
она живёт только во временном кэше ядра.

```python
client.new_chat(
    user_id=1,
    message="Что на картинке?",
    image=encode_image("cat.png"),
    save_media=True,        # постоянный чат: картинка переживёт перезапуск
)
```

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
