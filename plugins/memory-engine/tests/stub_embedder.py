"""A stand-in OpenAI-compatible embedding server, for the plugin's real-run test.

python3 -I stub_embedder.py DIR DIM. Binds 127.0.0.1 on a free port and writes it to DIR/port.

POST /v1/embeddings answers 200 with one entry per input, in order: a unit vector of DIM floats,
a hashed bag of the lowercased words of the input (empty input gets a vector whose first component
is 1.0), and a usage whose prompt_tokens and total_tokens are the total word count.

Any other method or path answers 404 with an empty body; a malformed JSON body on /v1/embeddings
answers 400. Both are logged in DIR/log as a "<ms since epoch> <METHOD> <path>" line.
"""

import hashlib
import http.server
import json
import math
import os
import re
import sys
import threading
import time

DIR = sys.argv[1]
DIM = int(sys.argv[2])
LOCK = threading.Lock()


def embed(text):
    v = [0.0] * DIM
    for word in re.findall(r"[a-z0-9]+", text.lower()):
        bucket = int.from_bytes(hashlib.sha256(word.encode()).digest()[:4], "big") % DIM
        v[bucket] += 1.0
    norm = math.sqrt(sum(c * c for c in v))
    if norm == 0.0:
        v[0] = 1.0
    else:
        v = [c / norm for c in v]
    return v


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def answer(self, status, body):
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log(self):
        with LOCK:
            with open(os.path.join(DIR, "log"), "a") as f:
                f.write("%d %s %s\n" % (time.time() * 1000, self.command, self.path))

    def dispatch(self):
        body = self.rfile.read(int(self.headers.get("Content-Length", "0")))
        if self.command != "POST" or self.path != "/v1/embeddings":
            self.log()
            self.answer(404, b"")
            return
        try:
            req = json.loads(body)
            inputs = req["input"]
            inputs = [inputs] if isinstance(inputs, str) else [str(t) for t in inputs]
        except (ValueError, KeyError, TypeError):
            self.log()
            self.answer(400, b"")
            return
        words = sum(len(re.findall(r"[a-z0-9]+", text.lower())) for text in inputs)
        data = [
            {"object": "embedding", "index": i, "embedding": embed(text)}
            for i, text in enumerate(inputs)
        ]
        self.answer(
            200,
            json.dumps(
                {
                    "object": "list",
                    "data": data,
                    "model": req.get("model", ""),
                    "usage": {"prompt_tokens": words, "total_tokens": words},
                }
            ).encode(),
        )

    do_GET = do_POST = do_PUT = do_DELETE = dispatch


server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
with open(os.path.join(DIR, "port.tmp"), "w") as f:
    f.write("%d\n" % server.server_address[1])
os.replace(os.path.join(DIR, "port.tmp"), os.path.join(DIR, "port"))
server.serve_forever()
