"""Ensure the browser fixture forwards its gateway only on permitted routes."""
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import threading
import unittest
from urllib.error import HTTPError
from urllib.request import urlopen

from browser_proxy import start_proxy


class Handler(BaseHTTPRequestHandler):
    paths = []

    def do_GET(self):
        self.paths.append(self.path)
        self.send_response(200)
        self.end_headers()
        self.wfile.write(b'ok')

    def log_message(self, *_args):
        pass


class BrowserProxyTests(unittest.TestCase):
    def test_gateway_allowlist(self):
        Handler.paths = []
        upstream = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        thread = threading.Thread(target=upstream.serve_forever, daemon=True)
        thread.start()
        proxy = start_proxy(upstream.server_port, upstream.server_port)
        base = f'http://127.0.0.1:{proxy.server_address[1]}'
        try:
            with urlopen(base + '/gateway/v1/workspaces?mux=herdr') as response:
                self.assertEqual(response.read(), b'ok')
            self.assertEqual(Handler.paths, ['/v1/workspaces?mux=herdr'])
            for route in ['/gateway/mata/api/simulators', '/gateway/v1/files/download',
                          '/gateway/v1/restart', '/gateway/v1/hosts/forward', '/api/private']:
                with self.assertRaises(HTTPError) as rejected:
                    urlopen(base + route)
                self.assertEqual(rejected.exception.code, 404)
                rejected.exception.close()
            self.assertEqual(Handler.paths, ['/v1/workspaces?mux=herdr'])
        finally:
            proxy.shutdown()
            proxy.server_close()
            upstream.shutdown()
            upstream.server_close()
            thread.join(timeout=2)


if __name__ == '__main__':
    unittest.main()
