"""Тесты `obscape.client` — HTTP-клиент на fake-сервере `http.server`."""

from __future__ import annotations

import json
import threading
import unittest
from http.server import BaseHTTPRequestHandler, HTTPServer

from obscape import ApiError, ObScapeClient, ObScapeError, encode_image


class _Handler(BaseHTTPRequestHandler):
    """Эндпоинты ядра в миниатюре; тело запроса кладётся в `last_body`."""

    server_version = "ObScapeTest/0.0.6"
    last_body: dict | None = None
    last_path = ""
    last_auth: str | None = None
    last_accept: str | None = None
    last_headers: dict | None = None
    health_status = 200
    stream_deltas = ["Hel", "lo"]
    stream_error: str | None = None

    def log_message(self, *args) -> None:  # noqa: D102 - тишина в тестах
        pass

    def _reply(self, status: int, payload: dict) -> None:
        body = json.dumps(payload).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _reply_sse(self) -> None:
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        deltas = _Handler.stream_deltas
        self.end_headers()
        if _Handler.stream_error is not None:
            _Handler.stream_error = None
            self.wfile.write(
                f"data: {json.dumps({'error': 'boom'})}\n\n".encode("utf-8")
            )
            return
        for delta in deltas:
            self.wfile.write(f"data: {json.dumps({'delta': delta})}\n\n".encode("utf-8"))
        final = {"chat_id": 42, "time": 7, "message": "".join(deltas), "done": True}
        self.wfile.write(f"data: {json.dumps(final)}\n\n".encode("utf-8"))

    def _remember(self) -> None:
        _Handler.last_path = self.path
        _Handler.last_auth = self.headers.get("Authorization")
        _Handler.last_accept = self.headers.get("Accept")
        _Handler.last_headers = dict(self.headers)

    def do_GET(self) -> None:
        self._remember()
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
        self._remember()
        if self.path in ("/v1/chat/new", "/v1/chat/message"):
            if "text/event-stream" in (self.headers.get("Accept") or ""):
                self._reply_sse()
            else:
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

    def test_image_and_save_media_are_sent(self) -> None:
        client = ObScapeClient(self.base_url, timeout=5)
        client.new_chat(user_id=1, message="what is this", image="data:image/png;base64,AAA", save_media=True)
        self.assertEqual(
            _Handler.last_body,
            {
                "user_id": 1,
                "message": "what is this",
                "image": "data:image/png;base64,AAA",
                "save_media": True,
            },
        )

    def test_no_media_fields_without_image(self) -> None:
        client = ObScapeClient(self.base_url, timeout=5)
        client.send_message(user_id=1, chat_id=2, message="hi")
        self.assertNotIn("image", _Handler.last_body)
        self.assertNotIn("save_media", _Handler.last_body)

    def test_stream_new_chat_yields_deltas_and_done(self) -> None:
        client = ObScapeClient(self.base_url, timeout=5)
        events = list(client.stream_new_chat(user_id=1, message="hi"))
        self.assertEqual([e.get("delta") for e in events[:-1]], ["Hel", "lo"])
        final = events[-1]
        self.assertTrue(final["done"])
        self.assertEqual(final["chat_id"], 42)
        self.assertEqual(final["message"], "Hello")
        self.assertEqual(_Handler.last_accept, "text/event-stream")

    def test_stream_message_sends_media_and_token(self) -> None:
        client = ObScapeClient(self.base_url, token="secret", timeout=5)
        list(
            client.stream_message(
                user_id=1, chat_id=2, message="hi", image="data:image/png;base64,AAA"
            )
        )
        self.assertEqual(_Handler.last_auth, "Bearer secret")
        self.assertEqual(_Handler.last_body["image"], "data:image/png;base64,AAA")

    def test_stream_surfaces_error_event(self) -> None:
        _Handler.stream_error = "boom"
        client = ObScapeClient(self.base_url, timeout=5)
        events = list(client.stream_new_chat(user_id=1, message="hi"))
        self.assertEqual(events, [{"error": "boom"}])

    def test_encode_image_builds_data_url(self) -> None:
        import os
        import tempfile

        with tempfile.NamedTemporaryFile(suffix=".png", delete=False) as f:
            f.write(b"\x89PNG-body")
            path = f.name
        try:
            url = encode_image(path)
        finally:
            os.unlink(path)
        self.assertTrue(url.startswith("data:image/png;base64,"))
        import base64 as b64

        self.assertEqual(b64.b64decode(url.split(",", 1)[1]), b"\x89PNG-body")


if __name__ == "__main__":
    unittest.main()
