"""A page to use a browser on: a form the agent fills in, and a receipt it reads back.

    python form_server.py <port> <log>

GET /form serves the form; POST /submit appends what was sent, with the browser's
User-Agent, to <log> as one JSON line and answers with a receipt number made at
that moment, so the agent can only know it by reading the page after submitting.
Every request is logged too ("GET /form ..."), on 127.0.0.1 only.
"""

import html
import json
import secrets
import sys
import urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PORT = int(sys.argv[1])
LOG = sys.argv[2]

FORM = """<!doctype html>
<html lang="vi"><head><meta charset="utf-8"><title>ha browser check</title>
<style>
body { font-family: Segoe UI, sans-serif; margin: 48px; font-size: 20px; }
label { display: block; margin: 18px 0 6px; }
input, select, button { font-size: 20px; padding: 6px 10px; }
input { width: 420px; }
button { margin-top: 24px; }
</style></head>
<body>
<h1>ha browser check</h1>
<form method="post" action="/submit">
  <label for="name">Name</label>
  <input id="name" name="name" autocomplete="off">
  <label for="colour">Colour</label>
  <select id="colour" name="colour">
    <option value="">choose one</option>
    <option value="red">red</option>
    <option value="green">green</option>
    <option value="blue">blue</option>
  </select>
  <div><button type="submit">Submit</button></div>
</form>
</body></html>
"""


def log(record):
    with open(LOG, "a", encoding="utf-8") as out:
        out.write(json.dumps(record, ensure_ascii=False) + "\n")


class Handler(BaseHTTPRequestHandler):
    def reply(self, body, status=200):
        data = body.encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(data)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        log({"request": "GET " + self.path, "agent": self.headers.get("User-Agent", "")})
        if self.path.split("?")[0] == "/form":
            self.reply(FORM)
        else:
            self.reply("<!doctype html><title>not here</title><p>Open /form.</p>", 404)

    def do_POST(self):
        length = int(self.headers.get("Content-Length") or 0)
        fields = urllib.parse.parse_qs(self.rfile.read(length).decode("utf-8"))
        receipt = "R-" + secrets.token_hex(3).upper()
        record = {
            "request": "POST " + self.path,
            "agent": self.headers.get("User-Agent", ""),
            "name": fields.get("name", [""])[0],
            "colour": fields.get("colour", [""])[0],
            "receipt": receipt,
        }
        log(record)
        self.reply(
            "<!doctype html><html><head><meta charset='utf-8'><title>ha browser check: received</title></head>"
            "<body style='font-family: Segoe UI; margin: 48px; font-size: 22px'>"
            f"<h1>Received</h1><p>Name: {html.escape(record['name'])}</p>"
            f"<p>Colour: {html.escape(record['colour'])}</p>"
            f"<p style='font-size: 32px'>Receipt number: <b>{receipt}</b></p></body></html>"
        )

    def log_message(self, *args):
        pass


ThreadingHTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
