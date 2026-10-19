"""Клиент ObScape: HTTP API чата и Unix-сокет управления конфигом.

Только стандартная библиотека — внешних зависимостей нет.

    from obscape import ObScapeClient, ConfigControl

    client = ObScapeClient("http://127.0.0.1:11080", token="...")
    print(client.health())
    chat = client.new_chat(user_id=1, message="Привет!")

    control = ConfigControl()          # ~/.config/obscape/control.sock
    control.reload()                   # перечитать config.toml на сервере
"""

from .client import ApiError, ObScapeClient, ObScapeError
from .control import ConfigControl, ControlError, default_socket_path

__all__ = [
    "ApiError",
    "ConfigControl",
    "ControlError",
    "ObScapeClient",
    "ObScapeError",
    "default_socket_path",
]

__version__ = "0.0.5"
