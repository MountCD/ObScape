"""HTTP-клиент ObScape: `/v1/health`, `/v1/chat/new`, `/v1/chat/message`."""

from __future__ import annotations

import base64
import json
import mimetypes
import urllib.error
import urllib.request
from typing import Any, Iterator, Optional

DEFAULT_BASE_URL = "http://127.0.0.1:11080"


def encode_image(path: str) -> str:
    """Прочитать файл и вернуть data-URL (`data:image/png;base64,...`).

    MIME определяется по расширению; неизвестное расширение отдаётся
    как `application/octet-stream` — ядро всё равно проверяет сигнатуру
    и вернёт 400, если формат не поддерживается.
    """
    mime, _ = mimetypes.guess_type(path)
    if not mime or not mime.startswith("image/"):
        mime = mime or "application/octet-stream"
    with open(path, "rb") as f:
        data = base64.b64encode(f.read()).decode("ascii")
    return f"data:{mime};base64,{data}"


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
        self,
        user_id: int,
        message: str,
        agent: Optional[str] = None,
        image: Optional[str] = None,
        save_media: bool = False,
    ) -> dict:
        """`POST /v1/chat/new` — новый чат с первым сообщением.

        ``agent`` — ключ из ``config.agents``; не указан → агент с
        ``main = true``. ``image`` — data-URL или голый base64 (см.
        :func:`encode_image`). ``save_media=True`` кладёт картинку в БД,
        иначе она живёт только во временном кэше. Возвращает
        ``{chat_id, time, message, tools}``.
        """
        body: dict = {"user_id": user_id, "message": message}
        if agent is not None:
            body["agent"] = agent
        _add_media(body, image, save_media)
        return self._request("POST", "/v1/chat/new", body)

    def send_message(
        self,
        user_id: int,
        chat_id: int,
        message: str,
        image: Optional[str] = None,
        save_media: bool = False,
    ) -> dict:
        """`POST /v1/chat/message` — сообщение в существующий чат."""
        body: dict = {"user_id": user_id, "chat_id": chat_id, "message": message}
        _add_media(body, image, save_media)
        return self._request("POST", "/v1/chat/message", body)

    def stream_new_chat(
        self,
        user_id: int,
        message: str,
        agent: Optional[str] = None,
        image: Optional[str] = None,
        save_media: bool = False,
    ) -> Iterator[dict]:
        """Как :meth:`new_chat`, но ответ приходит потоком SSE.

        Отдаёт словари по мере генерации: ``{"delta": "..."}`` для порций
        текста и финальный ``{"chat_id": ..., "time": ..., "message": ...,
        "done": True}``. Если модель оборвалась на середине — приходит
        ``{"error": "..."}`` и генератор заканчивается.
        """
        body: dict = {"user_id": user_id, "message": message}
        if agent is not None:
            body["agent"] = agent
        _add_media(body, image, save_media)
        return self._stream("/v1/chat/new", body)

    def stream_message(
        self,
        user_id: int,
        chat_id: int,
        message: str,
        image: Optional[str] = None,
        save_media: bool = False,
    ) -> Iterator[dict]:
        """Как :meth:`send_message`, но ответ приходит потоком SSE."""
        body: dict = {"user_id": user_id, "chat_id": chat_id, "message": message}
        _add_media(body, image, save_media)
        return self._stream("/v1/chat/message", body)

    def _stream(self, path: str, body: dict) -> Iterator[dict]:
        data = json.dumps(body).encode("utf-8")
        req = urllib.request.Request(self.base_url + path, data=data, method="POST")
        req.add_header("Content-Type", "application/json")
        req.add_header("Accept", "text/event-stream")
        if self.token:
            req.add_header("Authorization", f"Bearer {self.token}")
        try:
            resp = urllib.request.urlopen(req, timeout=self.timeout)
        except urllib.error.HTTPError as e:
            raw = e.read()
            e.close()
            payload = _try_json(raw)
            message = (
                str(payload["error"])
                if isinstance(payload, dict) and "error" in payload
                else (raw.decode("utf-8", errors="replace") or e.reason)
            )
            raise ApiError(e.code, message, payload) from None
        except (urllib.error.URLError, OSError) as e:
            raise ObScapeError(f"cannot reach {self.base_url}: {e}") from None

        with resp:
            # SSE: строки `data: {...}` до пустой строки-разделителя.
            # `for line in resp` буферизует, поэтому читаем построчно
            # сами — так дельты видны сразу.
            for raw_line in resp:
                line = raw_line.decode("utf-8", errors="replace").strip()
                if not line.startswith("data:"):
                    continue
                payload = _try_json(line[len("data:") :].strip().encode("utf-8"))
                if isinstance(payload, dict):
                    yield payload

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


def _add_media(body: dict, image: Optional[str], save_media: bool) -> None:
    """Добавить в тело запроса картинку и флаг её сохранения."""
    if image is not None:
        body["image"] = image
        body["save_media"] = bool(save_media)


def _try_json(raw: bytes) -> Any:
    try:
        return json.loads(raw or b"")
    except (ValueError, UnicodeDecodeError):
        return None
