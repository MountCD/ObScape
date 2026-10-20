# Сокет управления `ConfigControl`

Модуль `obscape.control`. Клиент Unix-сокета, который ядро держит открытым,
чтобы перечитать или применить конфиг без рестарта. Протокол — одна
JSON-команда на соединение, одна строка JSON в ответ (подробнее — в
[синхронизации с ядром](protocol.md)).

## Путь к сокету

`default_socket_path() -> Path` — как у сервера: каталог конфига из
`OBSISTENT_CONFIG`, иначе `$HOME/.config/obscape`, плюс `control.sock`.
Явное поле `control_socket` из `config.toml` клиенту неизвестно — в этом
случае передавайте путь вручную:

```python
ConfigControl("/run/obscape/control.sock")   # явный путь
ConfigControl()                              # OBSISTENT_CONFIG или ~/.config/obscape
```

## Конструктор

```python
ConfigControl(
    socket_path: Optional[Union[str, Path]] = None,
    timeout: float = 30.0,
)
```

`timeout` — сколько ждать ответа на одну команду (секунды). Команды
обрабатываются ядром синхронно на accept-цикле, поэтому таймаут 30 с обычно
с избытком.

## Методы

### `ping() -> dict`

Проверить, что сокет жив. Возвращает `{"ok": True, "service": "obscape"}`.

### `get() -> str`

Текущий конфиг ядра в виде TOML, **секреты заменены на `***`** (БД-пароль,
`api_key` каждого включённого агента, `api_token`).

Это снимок применённого конфига, а не содержимое файла: при запуске с
`--database` увидите переопределённый URL. Отдавать результат обратно в
`set` нельзя — сервер отклоняет маски.

### `get_dict() -> dict`

То же, но разобранное в словарь. Использует `tomllib` (Python 3.11+); на
более ранних версиях бросает `ControlError` с просьбой использовать `get()`.

### `reload() -> dict`

Перечитать `config.toml` с диска и применить. Ошибка валидации или
запрещённая в рантайме правка (`database_url`, `http_bind`, `control_socket`)
→ `ControlError`, прежний конфиг остаётся в силе.

### `set(config: str) -> dict`

Записать переданный TOML-текст в `config.toml` и применить без рестарта.
При любой ошибке (плохой TOML, неприменимые поля, маски `***`) файл
откатывается к прежнему содержимому — ядро остаётся на старом конфиге.

### `set_from_file(path: Union[str, Path]) -> dict`

Прислать в ядро содержимое файла (`Path.read_text(encoding="utf-8")`),
дальше как `set`.

`reload` и `set` не блокируют работающие HTTP-запросы: они меняют снимок
конфига в `RwLock`, который хэндлеры берут на каждый запрос.

## Ошибки

`ControlError` (подкласс `ObScapeError`) — сокет недоступен, сервер ответил
`{"ok": false, "error": ...}`, ответ не строка JSON, пустой ответ либо ответ
больше 4 МиБ (`MAX_REPLY_BYTES`).

```python
from obscape import ConfigControl, ControlError

control = ConfigControl()
try:
    control.set(Path("new_config.toml").read_text())
except ControlError as e:
    print("control failed:", e)
```

## Константы

| Имя | Значение |
| --- | --- |
| `DEFAULT_SOCKET_NAME` | `"control.sock"` |
| `DEFAULT_CONFIG_DIR` | `$HOME/.config/obscape` |
| `MAX_REPLY_BYTES` | 4 МиБ — предел чтения одного ответа |
