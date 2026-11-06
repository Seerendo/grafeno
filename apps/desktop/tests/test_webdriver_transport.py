"""Verifica recuperación de lecturas sin duplicar acciones de la aplicación."""

from http.client import RemoteDisconnected
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import threading
import unittest

from ui_smoke import Window


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_):
        pass

    def handle_command(self):
        self.rfile.read(int(self.headers.get("Content-Length", "0")))
        self.server.received.append((self.command, dict(self.headers)))
        if self.server.drop_first and len(self.server.received) == 1:
            # Simulates a command that arrived but whose acknowledgment was lost.
            self.close_connection = True
            return
        data = json.dumps({"value": self.server.payload}).encode()
        self.send_response(self.server.response_status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    do_GET = handle_command
    do_POST = handle_command


class TransportTests(unittest.TestCase):
    def window(self, *, payload=None, drop_first=False, status=200):
        server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        server.received = []
        server.drop_first = drop_first
        server.payload = {"ready": True} if payload is None else payload
        server.response_status = status
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        window = Window(f"http://127.0.0.1:{server.server_port}")
        window.session = "test"

        def cleanup():
            window.close()
            server.shutdown()
            server.server_close()
            thread.join(timeout=1)

        self.addCleanup(cleanup)
        return window, server

    def test_get_recovers_after_a_connection_is_closed(self):
        window, server = self.window(drop_first=True)
        self.assertEqual(window.command("GET", "/status"), {"ready": True})
        self.assertEqual(len(server.received), 2)
        self.assertNotEqual(server.received[-1][1].get("Connection"), "close")

    def test_dom_reads_can_be_retried(self):
        window, server = self.window(payload=True, drop_first=True)
        self.assertIs(window.read_dom("return true;"), True)
        self.assertEqual(len(server.received), 2)

    def test_a_click_is_not_repeated_when_its_acknowledgement_is_lost(self):
        window, server = self.window(drop_first=True)
        with self.assertRaises(RemoteDisconnected):
            window.call("POST", "/element/node/click", {})
        self.assertEqual(len(server.received), 1)

    def test_http_errors_are_not_hidden_by_retries(self):
        window, server = self.window(payload={"error": "invalid session", "message": "expired"}, status=400)
        with self.assertRaisesRegex(RuntimeError, "invalid session"):
            window.command("GET", "/status")
        self.assertEqual(len(server.received), 1)

    def test_application_errors_remain_available_as_test_data(self):
        expected = {"ok": False, "error": "database session changed"}
        window, server = self.window(payload=expected)
        self.assertEqual(window.read_dom("return {};"), expected)
        self.assertEqual(len(server.received), 1)


if __name__ == "__main__":
    unittest.main()
