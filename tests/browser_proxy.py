"""Loopback-only test proxy: stock Moshi assets with an isolated test gateway.

Moshi 0.3.21's web command hardcodes its gateway at port 24543. Keep browser
smoke tests off the real daemon by routing /gateway to the fixture's port.
"""
import select
import socket
import socketserver
import threading


def start_proxy(web_port, gateway_port):
    class Handler(socketserver.BaseRequestHandler):
        def handle(self):
            self.request.settimeout(10)
            data = b''
            while b'\r\n\r\n' not in data:
                part = self.request.recv(4096)
                if not part: return
                data += part
                if len(data) > 65536: return
            header, body = data.split(b'\r\n\r\n', 1)
            lines = header.split(b'\r\n')
            method, path, version = lines[0].split(b' ', 2)
            gateway = path.startswith(b'/gateway/')
            port = gateway_port if gateway else web_port
            if gateway: path = path[len(b'/gateway'):]
            upgrade = any(line.lower().startswith(b'upgrade: websocket') for line in lines)
            headers = [line for line in lines[1:] if not line.lower().startswith(b'host:')
                       and (upgrade or not line.lower().startswith(b'connection:'))]
            headers.append(f'Host: 127.0.0.1:{port}'.encode())
            if not upgrade: headers.append(b'Connection: close')
            with socket.create_connection(('127.0.0.1', port), timeout=10) as upstream:
                upstream.sendall(b' '.join([method, path, version]) + b'\r\n'
                                 + b'\r\n'.join(headers) + b'\r\n\r\n' + body)
                try:
                    while True:
                        readable, _, _ = select.select([self.request, upstream], [], [], 30)
                        if not readable: return
                        for source in readable:
                            chunk = source.recv(65536)
                            if not chunk: return
                            (upstream if source is self.request else self.request).sendall(chunk)
                except OSError:
                    return

    server = socketserver.ThreadingTCPServer(('127.0.0.1', 0), Handler)
    server.daemon_threads = True
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server
