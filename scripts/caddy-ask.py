#!/usr/bin/env python3
"""Allowlist HTTP service for Caddy on-demand TLS. Caddy: GET /allow?domain=host → 200 or 403."""
import os
import re
import sys
from http.server import BaseHTTPRequestHandler, HTTPServer
from urllib.parse import parse_qs, urlparse

NODE = os.environ.get("RELAY_NODE_FQDN", "").lower().strip().rstrip(".")
if not NODE:
    sys.exit("RELAY_NODE_FQDN is required")

# One label (repo) + node FQDN, or apex node FQDN only.
LABEL = r"[a-z0-9](?:[a-z0-9-]*[a-z0-9])?"
PAT = re.compile(
    rf"^(?:{LABEL}\.{re.escape(NODE)}|{re.escape(NODE)})$",
    re.I,
)


class Handler(BaseHTTPRequestHandler):
    def log_message(self, fmt, *args):
        pass

    def do_GET(self):
        q = parse_qs(urlparse(self.path).query)
        domain = (q.get("domain") or [""])[0].lower().strip().rstrip(".")
        ok = bool(domain and PAT.match(domain))
        self.send_response(200 if ok else 403)
        self.end_headers()


if __name__ == "__main__":
    HTTPServer(("127.0.0.1", 3999), Handler).serve_forever()
