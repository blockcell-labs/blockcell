"""Network-namespace-local TCP bridge to the supervisor's Unix socket."""
import os
import select
import socket
import socketserver
import subprocess
import sys
import threading


class Bridge(socketserver.BaseRequestHandler):
    def handle(self):
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as upstream:
            upstream.connect("/genesis_proxy.sock")
            while True:
                readable, _, _ = select.select([self.request, upstream], [], [], 180)
                if not readable:
                    return
                for src in readable:
                    data = src.recv(65536)
                    if not data:
                        return
                    (upstream if src is self.request else self.request).sendall(data)


if __name__ == "__main__":
    server = socketserver.ThreadingTCPServer(("127.0.0.1", 18788), Bridge)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        sys.exit(subprocess.call(sys.argv[1:], env=os.environ))
    finally:
        server.shutdown()
