#!/usr/bin/env python3
"""Shared Maildir inbox helper for relay-ops Inbox + batch SI consumers.

Canonical reader for the fleet inbound catcher (OpenSMTPD → Maildir).
Same list / get / mark-read semantics as relay-ops `/api/mail/*`.

Usage:
  maildir_inbox.py list <maildir>
  maildir_inbox.py get <maildir> <id>
  maildir_inbox.py mark-read <maildir> <id>

Prints one JSON document on stdout. Exit 2 if message id not found.
"""
from __future__ import annotations

import email
import json
import os
import re
import shutil
import sys
from email.header import decode_header, make_header
from html.parser import HTMLParser


def _dec(v: object) -> str:
    if not v:
        return ""
    try:
        return str(make_header(decode_header(v)))  # type: ignore[arg-type]
    except Exception:
        return v if isinstance(v, str) else str(v)


def _validate_id(mid: str) -> None:
    if not mid or len(mid) > 200:
        raise SystemExit("invalid mail id")
    if "/" in mid or "\\" in mid or ".." in mid or "\0" in mid:
        raise SystemExit("invalid mail id")
    for c in mid:
        if not (c.isalnum() or c in ".-_:"):
            raise SystemExit("invalid mail id")


class _Stripper(HTMLParser):
    def __init__(self) -> None:
        super().__init__()
        self.parts: list[str] = []

    def handle_data(self, d: str) -> None:
        self.parts.append(d)

    def text(self) -> str:
        return re.sub(r"\s+", " ", "".join(self.parts)).strip()


def _part_payload(part: email.message.Message) -> bytes:
    data = part.get_payload(decode=True)
    if data is None:
        p = part.get_payload()
        if isinstance(p, str):
            return p.encode("utf-8", "replace")
        return b""
    return data  # type: ignore[return-value]


def _as_text(part: email.message.Message) -> str:
    ctype = (part.get_content_type() or "").lower()
    charset = part.get_content_charset() or "utf-8"
    raw = _part_payload(part)
    try:
        text = raw.decode(charset, "replace")
    except Exception:
        text = raw.decode("utf-8", "replace")
    if ctype == "text/html":
        text = re.sub(r"(?is)<(script|style)[^>]*>.*?</\1>", " ", text)
        s = _Stripper()
        try:
            s.feed(text)
            return s.text()
        except Exception:
            return re.sub(r"<[^>]+>", " ", text)
    return text


def _find(root: str, mid: str) -> tuple[str, str]:
    for fol in ("new", "cur"):
        p = os.path.join(root, fol, mid)
        if os.path.isfile(p):
            return p, fol
    sys.stderr.write("not found\n")
    sys.exit(2)


def _move_to_cur(root: str, mid: str, path: str, folder: str) -> tuple[str, str]:
    if folder != "new":
        return path, folder
    dst_dir = os.path.join(root, "cur")
    os.makedirs(dst_dir, exist_ok=True)
    dst = os.path.join(dst_dir, mid)
    if not os.path.isfile(dst):
        shutil.move(path, dst)
    else:
        try:
            os.remove(path)
        except OSError:
            pass
    return dst, "cur"


def cmd_list(root: str) -> None:
    items = []
    for folder in ("new", "cur"):
        d = os.path.join(root, folder)
        if not os.path.isdir(d):
            continue
        for name in os.listdir(d):
            path = os.path.join(d, name)
            if not os.path.isfile(path):
                continue
            try:
                st = os.stat(path)
                with open(path, "rb") as f:
                    raw = f.read(65536)
                msg = email.message_from_bytes(raw)
                items.append(
                    {
                        "id": name,
                        "folder": folder,
                        "from": _dec(msg.get("From", "")),
                        "to": _dec(msg.get("To", "")),
                        "subject": _dec(msg.get("Subject", "")),
                        "date": _dec(msg.get("Date", "")),
                        "unread": folder == "new",
                        "mtime": float(st.st_mtime),
                    }
                )
            except Exception as e:
                items.append(
                    {
                        "id": name,
                        "folder": folder,
                        "from": "",
                        "to": "",
                        "subject": "(unreadable: %s)" % e,
                        "date": "",
                        "unread": folder == "new",
                        "mtime": 0.0,
                    }
                )
    items.sort(key=lambda x: x["mtime"], reverse=True)
    print(json.dumps(items))


def cmd_get(root: str, mid: str) -> None:
    _validate_id(mid)
    path, folder = _find(root, mid)
    path, folder = _move_to_cur(root, mid, path, folder)
    with open(path, "rb") as f:
        raw = f.read()
    msg = email.message_from_bytes(raw)
    plain: list[str] = []
    html: list[str] = []
    if msg.is_multipart():
        for part in msg.walk():
            if part.get_content_maintype() == "multipart":
                continue
            ctype = (part.get_content_type() or "").lower()
            if ctype == "text/plain":
                plain.append(_as_text(part))
            elif ctype == "text/html":
                html.append(_as_text(part))
    else:
        ctype = (msg.get_content_type() or "text/plain").lower()
        t = _as_text(msg)
        if ctype == "text/html":
            html.append(t)
        else:
            plain.append(t)
    body = "\n\n".join(plain).strip() or "\n\n".join(html).strip()
    print(
        json.dumps(
            {
                "id": mid,
                "folder": folder,
                "from": _dec(msg.get("From", "")),
                "to": _dec(msg.get("To", "")),
                "subject": _dec(msg.get("Subject", "")),
                "date": _dec(msg.get("Date", "")),
                "unread": False,
                "body": body,
            }
        )
    )


def cmd_mark_read(root: str, mid: str) -> None:
    _validate_id(mid)
    src = os.path.join(root, "new", mid)
    dst_dir = os.path.join(root, "cur")
    dst = os.path.join(dst_dir, mid)
    if not os.path.isfile(src):
        cur = os.path.join(dst_dir, mid)
        if os.path.isfile(cur):
            path, folder = cur, "cur"
        else:
            sys.stderr.write("not found\n")
            sys.exit(2)
    else:
        os.makedirs(dst_dir, exist_ok=True)
        shutil.move(src, dst)
        path, folder = dst, "cur"
    st = os.stat(path)
    with open(path, "rb") as f:
        raw = f.read(65536)
    msg = email.message_from_bytes(raw)
    print(
        json.dumps(
            {
                "id": mid,
                "folder": folder,
                "from": _dec(msg.get("From", "")),
                "to": _dec(msg.get("To", "")),
                "subject": _dec(msg.get("Subject", "")),
                "date": _dec(msg.get("Date", "")),
                "unread": False,
                "mtime": float(st.st_mtime),
            }
        )
    )


def main(argv: list[str] | None = None) -> None:
    args = list(sys.argv[1:] if argv is None else argv)
    if len(args) < 2:
        sys.stderr.write(
            "usage: maildir_inbox.py <list|get|mark-read> <maildir> [id]\n"
        )
        sys.exit(1)
    cmd, root = args[0], args[1]
    if cmd == "list":
        cmd_list(root)
    elif cmd == "get":
        if len(args) < 3:
            sys.stderr.write("usage: maildir_inbox.py get <maildir> <id>\n")
            sys.exit(1)
        cmd_get(root, args[2])
    elif cmd in ("mark-read", "mark_read"):
        if len(args) < 3:
            sys.stderr.write("usage: maildir_inbox.py mark-read <maildir> <id>\n")
            sys.exit(1)
        cmd_mark_read(root, args[2])
    else:
        sys.stderr.write("unknown command: %s\n" % cmd)
        sys.exit(1)


if __name__ == "__main__":
    main()
