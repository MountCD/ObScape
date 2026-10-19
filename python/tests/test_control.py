"""Тесты `obscape.control` — клиент Unix-сокета ядра."""

from __future__ import annotations

import json
import os
import socket
import tempfile
import threading
import unittest
from pathlib import Path

from obscape import ConfigControl, ControlError, default_socket_path
from obscape.control import DEFAULT_SOCKET_NAME


class FakeControlServer:
    """Мини-сервер по протоколу ядра: строка запроса -> строка ответа.

    `handler` получает разобранный запрос и возвращает словарь-ответ.
    """

    def __init__(self, directory: Path, handler):
        self.path = Path(directory) / DEFAULT_SOCKET_NAME
        self.handler = handler
        self.received: list[dict] = []
        self._listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self._listener.bind(str(self.path))
        self._listener.listen(4)
        self._closed = False
        self._thread = threading.Thread(target=self._serve, daemon=True)
        self._thread.start()

    def _serve(self) -> None:
        while True:
            try:
                conn, _ = self._listener.accept()
            except OSError:
                return  # сокет закрыт
            with conn:
                try:
                    data = b""
                    while b"\n" not in data:
                        chunk = conn.recv(65536)
                        if not chunk:
                            break
                        data += chunk
                    request = json.loads(data.split(b"\n", 1)[0].decode("utf-8"))
                    self.received.append(request)
                    reply = self.handler(request)
                except Exception as e:  # noqa: BLE001 - тестовый стенд
                    reply = {"ok": False, "error": f"handler failed: {e}"}
                conn.sendall(json.dumps(reply).encode("utf-8") + b"\n")

    def close(self) -> None:
        if self._closed:
            return
        self._closed = True
        self._listener.close()
        self._thread.join(timeout=2)


class ControlTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory(prefix="obscape-py-")
        self.addCleanup(self.tmp.cleanup)
        self.dir = Path(self.tmp.name)

    def start(self, handler) -> FakeControlServer:
        server = FakeControlServer(self.dir, handler)
        self.addCleanup(server.close)
        return server

    def test_ping_round_trip(self) -> None:
        self.start(lambda req: {"ok": True, "service": "obscape"})
        control = ConfigControl(self.dir / DEFAULT_SOCKET_NAME, timeout=5)
        self.assertEqual(control.ping(), {"ok": True, "service": "obscape"})

    def test_get_returns_config_text(self) -> None:
        toml_text = 'history_limit = 50\napi_key = "***"\n'
        self.start(lambda req: {"ok": True, "config": toml_text})
        control = ConfigControl(self.dir / DEFAULT_SOCKET_NAME, timeout=5)
        self.assertEqual(control.get(), toml_text)

    @unittest.skipUnless(
        __import__("importlib").util.find_spec("tomllib"), "нужен tomllib (3.11+)"
    )
    def test_get_dict_parses_toml(self) -> None:
        self.start(lambda req: {"ok": True, "config": "history_limit = 50\n"})
        control = ConfigControl(self.dir / DEFAULT_SOCKET_NAME, timeout=5)
        self.assertEqual(control.get_dict(), {"history_limit": 50})

    def test_reload_and_set_send_expected_commands(self) -> None:
        server = self.start(lambda req: {"ok": True})
        control = ConfigControl(self.dir / DEFAULT_SOCKET_NAME, timeout=5)

        control.reload()
        control.set("history_limit = 7\n")
        self.assertEqual(server.received, [{"cmd": "reload"}, {"cmd": "set", "config": "history_limit = 7\n"}])

    def test_set_from_file_reads_file(self) -> None:
        server = self.start(lambda req: {"ok": True})
        cfg_file = self.dir / "config.toml"
        cfg_file.write_text("shared_prompt = \"hi\"\n", encoding="utf-8")

        control = ConfigControl(self.dir / DEFAULT_SOCKET_NAME, timeout=5)
        control.set_from_file(cfg_file)
        self.assertEqual(server.received[-1], {"cmd": "set", "config": "shared_prompt = \"hi\"\n"})

    def test_server_error_becomes_control_error(self) -> None:
        self.start(lambda req: {"ok": False, "error": "masked values are not accepted"})
        control = ConfigControl(self.dir / DEFAULT_SOCKET_NAME, timeout=5)
        with self.assertRaises(ControlError) as ctx:
            control.set("api_key = \"***\"\n")
        self.assertIn("masked values", str(ctx.exception))

    def test_missing_socket_becomes_control_error(self) -> None:
        control = ConfigControl(self.dir / DEFAULT_SOCKET_NAME, timeout=2)
        with self.assertRaises(ControlError) as ctx:
            control.ping()
        self.assertIn("cannot reach", str(ctx.exception))

    def test_malformed_reply_becomes_control_error(self) -> None:
        # Сервер шлёт не-JSON: клиент не должен падать с ValueError.
        listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        path = self.dir / DEFAULT_SOCKET_NAME
        listener.bind(str(path))
        listener.listen(1)

        def _serve() -> None:
            conn, _ = listener.accept()
            with conn:
                conn.recv(65536)
                conn.sendall(b"definitely not json\n")

        thread = threading.Thread(target=_serve, daemon=True)
        thread.start()
        self.addCleanup(listener.close)

        control = ConfigControl(path, timeout=5)
        with self.assertRaises(ControlError) as ctx:
            control.ping()
        self.assertIn("malformed reply", str(ctx.exception))


class DefaultPathTests(unittest.TestCase):
    def test_env_overrides_config_dir(self) -> None:
        old = os.environ.get("OBSISTENT_CONFIG")
        os.environ["OBSISTENT_CONFIG"] = "/tmp/custom-obscape"
        try:
            self.assertEqual(
                default_socket_path(), Path("/tmp/custom-obscape") / DEFAULT_SOCKET_NAME
            )
        finally:
            if old is None:
                del os.environ["OBSISTENT_CONFIG"]
            else:
                os.environ["OBSISTENT_CONFIG"] = old

    def test_falls_back_to_home_config_dir(self) -> None:
        old = os.environ.pop("OBSISTENT_CONFIG", None)
        try:
            self.assertEqual(
                default_socket_path(), Path.home() / ".config" / "obscape" / DEFAULT_SOCKET_NAME
            )
        finally:
            if old is not None:
                os.environ["OBSISTENT_CONFIG"] = old


if __name__ == "__main__":
    unittest.main()
