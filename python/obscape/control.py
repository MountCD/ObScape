"""Клиент Unix-сокета управления ObScape: `get` / `reload` / `set` / `ping`.

Сокет поднимает ядро при старте (по умолчанию `<каталог конфига>/control.sock`,
путь переопределяется полем `control_socket` в `config.toml`). Протокол — одна
JSON-команда на соединение, ответом идёт одна строка JSON:

    {"cmd":"ping"}                      -> {"ok":true,"service":"obscape"}
    {"cmd":"get"}                       -> {"ok":true,"config":"<TOML>"}
    {"cmd":"reload"}                    -> {"ok":true}
    {"cmd":"set","config":"<TOML>"}     -> {"ok":true}

`get` возвращает конфиг с масками секретов (`***`) — только для чтения,
отправлять его обратно в `set` сервер отклоняет.
"""

from __future__ import annotations

import json
import os
import socket
from pathlib import Path
from typing import Any, Optional, Union

from .client import ObScapeError

#: Имя сокета внутри каталога конфига.
DEFAULT_SOCKET_NAME = "control.sock"

#: Дефолтный каталог конфига — как у ядра (`$HOME/.config/obscape`).
DEFAULT_CONFIG_DIR = Path.home() / ".config" / "obscape"

#: Максимум, что мы готовы прочитать из сокета за один ответ.
MAX_REPLY_BYTES = 4 * 1024 * 1024


class ControlError(ObScapeError):
    """Сокет недоступен либо сервер ответил `{"ok": false, ...}`."""


def default_socket_path() -> Path:
    """Путь к сокету, который слушает ядро.

    Как и у сервера, источник истины — каталог конфига: переменная
    `OBSISTENT_CONFIG` (по ней же стартует `ob_core`), иначе
    `$HOME/.config/obscape`. Явный `control_socket` из `config.toml`
    клиенту не известен — передавайте его вручную.
    """
    env_dir = os.environ.get("OBSISTENT_CONFIG")
    config_dir = Path(env_dir) if env_dir else DEFAULT_CONFIG_DIR
    return config_dir / DEFAULT_SOCKET_NAME


class ConfigControl:
    """Подгрузка конфига работающего ядра через Unix-сокет.

    >>> control = ConfigControl()               # ~/.config/obscape/control.sock
    >>> control.ping()
    {'ok': True, 'service': 'obscape'}
    >>> toml_text = control.get()               # конфиг с масками, str
    >>> control.reload()                        # перечитать config.toml с диска
    >>> control.set(Path("new_config.toml").read_text())

    `timeout` — сколько ждать ответа на одну команду (секунды). `set` и
    `reload` не блокируют работающие запросы: они меняют снимок конфига,
    который хэндлеры берут на каждый запрос.
    """

    def __init__(
        self,
        socket_path: Optional[Union[str, Path]] = None,
        timeout: float = 30.0,
    ):
        self.socket_path = Path(socket_path) if socket_path else default_socket_path()
        self.timeout = timeout

    def ping(self) -> dict:
        """Проверить, что сокет жив и отвечает. Возвращает `{"service": ...}`."""
        return self._call({"cmd": "ping"})

    def get(self) -> str:
        """Текущий конфиг ядра в виде TOML (секреты заменены на `***`).

        Это снимок применённого конфига, а не содержимое файла: если сервер
        запущен с `--database`, увидите переопределённый URL. Отдавать
        результат обратно в `set` нельзя — сервер отклонит маски.
        """
        return self._call({"cmd": "get"})["config"]

    def get_dict(self) -> dict:
        """То же, что [`get`], но разобранное в словарь (нужен `tomllib`).

        `tomllib` появился в Python 3.11; до этого читайте TOML сами.
        """
        try:
            import tomllib
        except ModuleNotFoundError as e:  # pragma: no cover - зависит от версии
            raise ControlError(
                "get_dict() requires Python 3.11+ (tomllib); use get() instead"
            ) from e
        return tomllib.loads(self.get())

    def reload(self) -> dict:
        """Перечитать `config.toml` с диска и применить его.

        Ошибка валидации или запрещённая в рантайме правка (`database_url`,
        `http_bind`, `control_socket`) вернёт `ControlError`, а прежний
        конфиг останется в силе.
        """
        return self._call({"cmd": "reload"})

    def set(self, config: str) -> dict:
        """Записать `config` в `config.toml` и применить без рестарта.

        При любой ошибке (плохой TOML, неприменимые поля) файл возвращается
        к прежнему содержимому — ядро остаётся на старом конфиге.
        """
        return self._call({"cmd": "set", "config": config})

    def set_from_file(self, path: Union[str, Path]) -> dict:
        """Прислать в ядро содержимое файла с конфигом."""
        return self.set(Path(path).read_text(encoding="utf-8"))

    def _call(self, command: dict) -> dict:
        """Отправить одну команду и разобрать ответ."""
        request = json.dumps(command, ensure_ascii=False).encode("utf-8") + b"\n"
        try:
            reply = self._exchange(request)
        except OSError as e:
            raise ControlError(f"cannot reach control socket {self.socket_path}: {e}") from e

        if not isinstance(reply, dict):
            raise ControlError(f"unexpected reply from {self.socket_path}: {reply!r}")
        if not reply.get("ok"):
            raise ControlError(str(reply.get("error") or "control command failed"))
        return reply

    def _exchange(self, request: bytes) -> Any:
        """Одно соединение: отправили строку, читали строку, закрыли."""
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
            sock.settimeout(self.timeout)
            sock.connect(str(self.socket_path))
            sock.sendall(request)

            chunks: list[bytes] = []
            received = 0
            while b"\n" not in (tail := b"".join(chunks)):
                chunk = sock.recv(65536)
                if not chunk:
                    break
                received += len(chunk)
                if received > MAX_REPLY_BYTES:
                    raise ControlError(
                        f"reply from {self.socket_path} exceeds {MAX_REPLY_BYTES} bytes"
                    )
                chunks.append(chunk)
            payload = tail.split(b"\n", 1)[0]

        if not payload:
            raise ControlError(f"no reply from {self.socket_path}")
        try:
            return json.loads(payload.decode("utf-8"))
        except (ValueError, UnicodeDecodeError) as e:
            raise ControlError(
                f"malformed reply from {self.socket_path}: {payload[:200]!r}"
            ) from e
