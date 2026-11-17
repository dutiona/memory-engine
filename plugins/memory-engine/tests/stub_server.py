"""A stand-in for the memory-engine event route, for the plugin's tests.

python3 -I stub_server.py DIR. Binds 127.0.0.1 on a free port and writes it to DIR/port. Every answer is read from
files in DIR at request time, so a test changes the stub's behaviour by writing them:

- DIR/status: the status for POST /lifecycle/events (default 204);
- DIR/token: when present, any other Authorization than "Bearer <token>" gets 401.

Each request is saved as DIR/req/<n>.headers and DIR/req/<n>.body, and logged in DIR/log as
"<ms since epoch> <status> <ME-Event-Id>".
"""

import http.server
import os
import sys
import threading
import time

DIR = sys.argv[1]
LOCK = threading.Lock()
STATE = {"n": 0}


def read(name, default):
    try:
        with open(os.path.join(DIR, name)) as f:
            return f.read().strip()
    except OSError:
        return default


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def answer(self, status, extra=()):
        self.send_response(status)
        for name, value in extra:
            self.send_header(name, value)
        if status not in (204, 304):
            self.send_header("Content-Length", "0")
        self.end_headers()

    def do_POST(self):
        body = self.rfile.read(int(self.headers.get("Content-Length", "0")))
        token = read("token", None)
        if self.path != "/lifecycle/events":
            status = 404
        elif (
            token is not None and self.headers.get("Authorization") != "Bearer " + token
        ):
            status = 401
        else:
            status = int(read("status", "204"))
        event_id = self.headers.get("ME-Event-Id", "-")
        with LOCK:
            STATE["n"] += 1
            n = STATE["n"]
            os.makedirs(os.path.join(DIR, "req"), exist_ok=True)
            with open(os.path.join(DIR, "req", "%d.headers" % n), "w") as f:
                f.write(str(self.headers))
            with open(os.path.join(DIR, "req", "%d.body" % n), "wb") as f:
                f.write(body)
            with open(os.path.join(DIR, "log"), "a") as f:
                f.write("%d %d %s\n" % (time.time() * 1000, status, event_id))
        extra = []
        if 300 <= status < 400:
            extra.append(("Location", "/moved"))
        self.answer(status, extra)

    def do_GET(self):
        self.answer(404)


server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
with open(os.path.join(DIR, "port.tmp"), "w") as f:
    f.write("%d\n" % server.server_address[1])
os.replace(os.path.join(DIR, "port.tmp"), os.path.join(DIR, "port"))
server.serve_forever()
