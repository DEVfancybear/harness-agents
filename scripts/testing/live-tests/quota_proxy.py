"""A pass-through proxy to the ChatGPT Codex backend that can refuse with a
usage limit, for the live tests of a quota park.

    python quota_proxy.py <dir>

It listens on 127.0.0.1 (the port is written to <dir>/port) and forwards every
request to https://chatgpt.com unchanged. While <dir>/limit exists, a request
is answered instead with HTTP 429 and ChatGPT's `usage_limit_reached` body,
resetting four hours from now. Each request is logged to <dir>/requests.jsonl
as {"ms", "status", "key", "body_has"} - `key` is the request's
prompt_cache_key (one per conversation), `body_has` the words of <dir>/watch
that the request body holds. Nothing of the request (its credentials
included) is written anywhere else.
"""

import http.server
import json
import os
import socketserver
import sys
import time
import urllib.error
import urllib.request

UPSTREAM = "https://chatgpt.com"
HOP = {"host", "content-length", "connection", "accept-encoding", "transfer-encoding"}


def main() -> None:
    directory = sys.argv[1]
    os.makedirs(directory, exist_ok=True)

    def log(status, body):
        key = None
        try:
            key = json.loads(body).get("prompt_cache_key")
        except (ValueError, AttributeError):
            pass
        watch = []
        try:
            with open(os.path.join(directory, "watch"), encoding="utf-8") as handle:
                watch = handle.read().split()
        except OSError:
            pass
        text = body.decode("utf-8", errors="replace")
        row = {
            "ms": int(time.time() * 1000),
            "status": status,
            "key": key,
            "body_has": [word for word in watch if word in text],
        }
        with open(os.path.join(directory, "requests.jsonl"), "a", encoding="utf-8") as handle:
            handle.write(json.dumps(row) + "\n")

    class Handler(http.server.BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.0"

        def log_message(self, *args):
            pass

        def do_POST(self):
            length = int(self.headers.get("content-length") or 0)
            body = self.rfile.read(length)
            if os.path.exists(os.path.join(directory, "limit")):
                resets = int(time.time()) + 4 * 3600
                answer = json.dumps(
                    {
                        "error": {
                            "type": "usage_limit_reached",
                            "message": "The usage limit has been reached",
                            "plan_type": "plus",
                            "resets_at": resets,
                        }
                    }
                ).encode()
                log(429, body)
                self.send_response(429)
                self.send_header("content-type", "application/json")
                self.send_header("content-length", str(len(answer)))
                self.end_headers()
                self.wfile.write(answer)
                return
            headers = {k: v for k, v in self.headers.items() if k.lower() not in HOP}
            request = urllib.request.Request(
                UPSTREAM + self.path, data=body or None, headers=headers, method=self.command
            )
            try:
                upstream = urllib.request.urlopen(request, timeout=600)
                status = upstream.status
            except urllib.error.HTTPError as error:
                upstream, status = error, error.code
            log(status, body)
            self.send_response(status)
            for key, value in upstream.headers.items():
                if key.lower() not in HOP:
                    self.send_header(key, value)
            self.end_headers()
            while True:
                chunk = upstream.read1(65536) if hasattr(upstream, "read1") else upstream.read(65536)
                if not chunk:
                    break
                self.wfile.write(chunk)
                self.wfile.flush()

        do_GET = do_POST

    class Server(socketserver.ThreadingMixIn, http.server.HTTPServer):
        daemon_threads = True

    server = Server(("127.0.0.1", 0), Handler)
    with open(os.path.join(directory, "port"), "w", encoding="utf-8") as handle:
        handle.write(str(server.server_address[1]))
    server.serve_forever()


if __name__ == "__main__":
    main()
