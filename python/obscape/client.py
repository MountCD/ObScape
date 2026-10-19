"""HTTP-клиент ObScape: `/v1/health`, `/v1/chat/new`, `/v1/chat/message`."""

from __future__ import annotations

import json
import urllib.error
import urllib.request
from typing import Any, Optional

DEFAULT_BASE_URL = "http://127.0.0.1:11080"


class ObScapeError(Exception):
    """Базовая ошибка библиотеки."""


class ApiError(ObScapeError):
    """Сервер ответил ошибкой (HTTP >= 400)."""

    def __init__(self, status: int, message: str, payload: Any = None):
        super().__init__(f"HTTP {status}: {message}")
        self.status = status
        self.message = message
        self.payload = payload


class ObScapeClient:
    """Клиент HTTP API ядра.

    ``token`` — bearer-токен ``api_token`` из конфига; если сервер без
    токена, оставляйте ``None``. ``timeout`` — таймаут одного запроса
    (ответ модели может идти долго, дефолт 180 c — как у сервера).
    """

    def __init__(
        self,
        base_url: str = DEFAULT_BASE_URL,
        token: Optional[str] = None,
        timeout: float = 180.0,
    ):
        self.base_url = base_url.rstrip("/")
        self.token = token
        self.timeout = timeout

    def health(self) -> dict:
        """`GET /v1/health`. Ответ 503 (`degraded`) — нормальный исход:
        он возвращается как словарь, а не как ошибка."""
        try:
            return self._request("GET", "/v1/health")
        except ApiError as e:
            if e.status == 503 and isinstance(e.payload, dict):
                return e.payload
            raise

    def new_chat(
        self, user_id: int, message: str, agent: Optional[str] = None
    ) -> dict:
        """`POST /v1/chat/new` — новый чат с первым сообщением.

        ``agent`` — ключ из ``config.agents``; не указан → агент с
        ``main = true``. Возвращает ``{chat_id, time, message, tools}``.
        """
        body: dict = {"user_id": user_id, "message": message}
        if agent is not None:
            body["agent"] = agent
        return self._request("POST", "/v1/chat/new", body)

    def send_message(self, user_id: int, chat_id: int, message: str) -> dict:
        """`POST /v1/chat/message` — сообщение в существующий чат."""
        return self._request(
            "POST",
            "/v1/chat/message",
            {"user_id": user_id, "chat_id": chat_id, "message": message},
        )

    def _request(self, method: str, path: str, body: Optional[dict] = None) -> dict:
        data = json.dumps(body).encode("utf-8") if body is not None else None
        req = urllib.request.Request(self.base_url + path, data=data, method=method)
        if data is not None:
            req.add_header("Content-Type", "application/json")
        if self.token:
            req.add_header("Authorization", f"Bearer {self.token}")
        try:
            with urllib.request.urlopen(req, timeout=self.timeout) as resp:
                raw = resp.read()
        except urllib.error.HTTPError as e:
            raw = e.read()
            e.close()
            payload = _try_json(raw)
            if isinstance(payload, dict) and "error" in payload:
                message = str(payload["error"])
            else:
                message = raw.decode("utf-8", errors="replace") or e.reason
            raise ApiError(e.code, message, payload) from None
        except (urllib.error.URLError, OSError) as e:
            raise ObScapeError(f"cannot reach {self.base_url}: {e}") from None
        parsed = _try_json(raw)
        if not isinstance(parsed, dict):
            raise ObScapeError(f"unexpected response from {self.base_url}{path}: {raw!r}")
        return parsed


def _try_json(raw: bytes) -> Any:
    try:
        return json.loads(raw or b"")
    except (ValueError, UnicodeDecodeError):
        return None
