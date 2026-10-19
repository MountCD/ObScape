"""Тесты `obscape.client` — HTTP-клиент на fake-сервере `http.server`."""

from __future__ import annotations

import json
import threading
import unittest
from http.server import BaseHTTPRequestHandler, HTTPServer

from obscape import ApiError, ObScapeClient, ObScapeError


class _Handler(BaseHTTPRequestHandler):
    """Эндпоинты ядра в миниатюре; тело запроса кладётся в `last_body`."""

    server_version = "ObScapeTest/0.0.5"
    last_body: dict | None = None
    last_path = ""
    last_auth: str | None = None
    health_status = 200

    def log_message(self, *args) -> None:  # noqa: D102 - тишина в тестах
        pass

    def _reply(self, status: int, payload: dict) -> None:
        body = json.dumps(payload).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self) -> None:
        _Handler.last_path = self.path
        _Handler.last_auth = self.headers.get("Authorization")
        if self.path == "/v1/health":
            self._reply(
                _Handler.health_status,
                {"status": "degraded", "db": "down"}
                if _Handler.health_status == 503
                else {"status": "ok", "db": "up"},
            )
        else:
            self._reply(404, {"error": "not found"})

    def do_POST(self) -> None:
        length = int(self.headers.get("Content-Length") or 0)
        raw = self.rfile.read(length) if length else b""
        _Handler.last_body = json.loads(raw) if raw else None
        _Handler.last_path = self.path
        _Handler.last_auth = self.headers.get("Authorization")
        if self.path in ("/v1/chat/new", "/v1/chat/message"):
            self._reply(200, {"chat_id": 42, "time": "2026-01-01T00:00:00Z", "message": "ok"})
        else:
            self._reply(404, {"error": "not found"})


class ClientTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.server = HTTPServer(("127.0.0.1", 0), _Handler)
        cls.thread = threading.Thread(target=cls.server.serve_forever, daemon=True)
        cls.thread.start()
        cls.base_url = f"http://127.0.0.1:{cls.server.server_address[1]}"

    @classmethod
    def tearDownClass(cls) -> None:
        cls.server.shutdown()
        cls.server.server_close()
        cls.thread.join(timeout=2)

    def setUp(self) -> None:
        _Handler.last_body = None
        _Handler.last_path = ""
        _Handler.last_auth = None
        _Handler.health_status = 200

    def test_health_ok(self) -> None:
        client = ObScapeClient(self.base_url, timeout=5)
        self.assertEqual(client.health(), {"status": "ok", "db": "up"})

    def test_health_503_is_returned_not_raised(self) -> None:
        _Handler.health_status = 503
        client = ObScapeClient(self.base_url, timeout=5)
        self.assertEqual(client.health(), {"status": "degraded", "db": "down"})

    def test_new_chat_sends_body_and_token(self) -> None:
        client = ObScapeClient(self.base_url, token="secret", timeout=5)
        reply = client.new_chat(user_id=7, message="привет")
        self.assertEqual(reply["chat_id"], 42)
        self.assertEqual(_Handler.last_path, "/v1/chat/new")
        self.assertEqual(_Handler.last_body, {"user_id": 7, "message": "привет"})
        self.assertEqual(_Handler.last_auth, "Bearer secret")

    def test_new_chat_with_explicit_agent(self) -> None:
        client = ObScapeClient(self.base_url, timeout=5)
        client.new_chat(user_id=1, message="hi", agent="primary")
        self.assertEqual(_Handler.last_body, {"user_id": 1, "message": "hi", "agent": "primary"})
        self.assertIsNone(_Handler.last_auth)

    def test_send_message(self) -> None:
        client = ObScapeClient(self.base_url, timeout=5)
        client.send_message(user_id=7, chat_id=42, message="ещё")
        self.assertEqual(_Handler.last_path, "/v1/chat/message")
        self.assertEqual(
            _Handler.last_body, {"user_id": 7, "chat_id": 42, "message": "ещё"}
        )

    def test_http_error_becomes_api_error(self) -> None:
        client = ObScapeClient(self.base_url, timeout=5)
        with self.assertRaises(ApiError) as ctx:
            client._request("GET", "/nope")
        self.assertEqual(ctx.exception.status, 404)
        self.assertEqual(ctx.exception.payload, {"error": "not found"})

    def test_unreachable_server_becomes_obscape_error(self) -> None:
        client = ObScapeClient("http://127.0.0.1:1", timeout=1)
        with self.assertRaises(ObScapeError) as ctx:
            client.health()
        self.assertIn("cannot reach", str(ctx.exception))


if __name__ == "__main__":
    unittest.main()
