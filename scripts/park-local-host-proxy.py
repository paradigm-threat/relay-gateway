#!/usr/bin/env python3
"""Tiny Host-injecting proxy for park-local browser iteration.

  http://127.0.0.1:9081/pt/…  → Host: paradigmthreat.net
  http://127.0.0.1:9081/fb/…  → Host: flashbackrepository.org
  http://127.0.0.1:9081/ct/…  → Host: clevertree.net

Upstream: RELAY_HTTP_PORT (default 9080).
"""
from __future__ import annotations

import http.client
import os
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

UPSTREAM = ("127.0.0.1", int(os.environ.get("RELAY_HTTP_PORT", "9080")))
LISTEN = int(os.environ.get("PARK_PROXY_PORT", "9081"))

PREFIXES = {
    "/pt": "paradigmthreat.net",
    "/fb": "flashbackrepository.org",
    "/ct": "clevertree.net",
}


class Handler(BaseHTTPRequestHandler):
    def log_message(self, fmt: str, *args) -> None:
        sys.stderr.write("[park-proxy] " + (fmt % args) + "\n")

    def _proxy(self) -> None:
        path = self.path.split("?", 1)[0]
        host = None
        upstream_path = self.path
        for prefix, h in PREFIXES.items():
            if path == prefix or path.startswith(prefix + "/"):
                host = h
                rest = path[len(prefix) :] or "/"
                qs = ""
                if "?" in self.path:
                    qs = "?" + self.path.split("?", 1)[1]
                upstream_path = rest + qs
                break
        if host is None:
            self.send_response(200)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.end_headers()
            self.wfile.write(
                b"""<!DOCTYPE html><html><body>
<h1>park-local Host proxy</h1>
<ul>
<li><a href="/pt/">Paradigm Threat</a></li>
<li><a href="/fb/">Flashback Repository</a></li>
<li><a href="/ct/">Clevertree</a></li>
</ul>
<p>Gateway must be on :%d</p>
</body></html>"""
                % UPSTREAM[1]
            )
            return

        conn = http.client.HTTPConnection(*UPSTREAM, timeout=120)
        body = None
        if self.command in ("POST", "PUT", "QUERY", "PATCH"):
            length = int(self.headers.get("Content-Length") or 0)
            body = self.rfile.read(length) if length else None
        headers = {k: v for k, v in self.headers.items() if k.lower() not in ("host",)}
        headers["Host"] = host
        headers["Accept"] = headers.get("Accept") or "text/html,*/*"
        try:
            conn.request(self.command, upstream_path, body=body, headers=headers)
            resp = conn.getresponse()
            data = resp.read()
        except Exception as e:
            self.send_error(502, f"upstream: {e}")
            return
        finally:
            conn.close()

        self.send_response(resp.status)
        for k, v in resp.getheaders():
            lk = k.lower()
            if lk in ("transfer-encoding", "connection", "content-length"):
                continue
            # Rewrite absolute park URLs so in-page links stay on the proxy.
            if lk == "location" and v.startswith("http"):
                continue
            self.send_header(k, v)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self) -> None:
        self._proxy()

    def do_HEAD(self) -> None:
        self._proxy()

    def do_POST(self) -> None:
        self._proxy()

    def do_QUERY(self) -> None:
        self._proxy()


def main() -> None:
    http.server.BaseHTTPRequestHandler.protocol_version = "HTTP/1.1"
    srv = ThreadingHTTPServer(("127.0.0.1", LISTEN), Handler)
    print(f"[park-proxy] http://127.0.0.1:{LISTEN}/  → gateway {UPSTREAM[0]}:{UPSTREAM[1]}", flush=True)
    srv.serve_forever()


if __name__ == "__main__":
    # Allow QUERY method name
    if not hasattr(BaseHTTPRequestHandler, "do_QUERY"):
        BaseHTTPRequestHandler.do_QUERY = Handler.do_QUERY  # type: ignore
    main()
