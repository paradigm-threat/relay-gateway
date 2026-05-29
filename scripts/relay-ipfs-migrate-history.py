#!/usr/bin/env python3
"""
Pin every binary blob in full git history and produce filter-repo callback by object id.

  relay-ipfs-migrate-history.py /path/to/repo --workdir /path/to/work

Outputs in workdir:
  oid-stubs.json, head-paths.csv, blob-callback.py, assets.manifest.json
"""
from __future__ import annotations

import argparse
import csv
import hashlib
import json
import subprocess
import sys
import tempfile
from pathlib import Path

POINTER_MAGIC = "relay-ipfs-v1"
MANIFEST = "assets.manifest.json"
SCHEMA = "relay.assets-manifest.v1"

BINARY_EXT = {
    ".png", ".jpg", ".jpeg", ".gif", ".webp", ".ico", ".pdf", ".zip",
    ".mp3", ".mp4", ".webm", ".mkv", ".avi", ".mov",
    ".woff", ".woff2", ".ttf", ".eot", ".doc", ".docx", ".epub",
}


def run(cmd: list[str], **kw) -> subprocess.CompletedProcess:
    kw.setdefault("text", True)
    kw.setdefault("encoding", "utf-8")
    kw.setdefault("errors", "surrogateescape")
    return subprocess.run(cmd, check=False, capture_output=True, **kw)


def is_binary(path_hint: str, data: bytes) -> bool:
    if not data or data.startswith(b"relay-ipfs-v1\n"):
        return False
    if isinstance(path_hint, bytes):
        path_hint = path_hint.decode("utf-8", "surrogateescape")
    ext = Path(path_hint).suffix.lower() if path_hint else ""
    if ext in {".md", ".svg", ".txt", ".yaml", ".yml", ".html", ".css", ".js", ".cs"}:
        if b"\0" not in data:
            try:
                data.decode("utf-8")
                return False
            except UnicodeDecodeError:
                pass
    if b"\0" in data:
        return True
    if ext in BINARY_EXT and len(data) > 64:
        return True
    try:
        data.decode("utf-8")
        return False
    except UnicodeDecodeError:
        return len(data) > 256


def guess_mime(path: str) -> str:
    import mimetypes
    t, _ = mimetypes.guess_type(path)
    return t or "application/octet-stream"


def ipfs_add(data: bytes, suffix: str = ".bin") -> str:
    with tempfile.NamedTemporaryFile(delete=False, suffix=suffix) as f:
        f.write(data)
        tmp = f.name
    try:
        proc = run(["ipfs", "add", "--pin", "--quieter", "--cid-version=1", tmp])
        if proc.returncode != 0:
            raise RuntimeError(proc.stderr or proc.stdout or "ipfs add failed")
        return proc.stdout.strip().split()[-1]
    finally:
        Path(tmp).unlink(missing_ok=True)


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def make_stub(cid: str, mime: str, sha: str) -> str:
    return f"{POINTER_MAGIC}\n{cid}\n{mime}\nsha256={sha}\n"


def iter_all_blobs(repo: Path) -> list[tuple[str, str]]:
    proc = run(["git", "-C", str(repo), "rev-list", "--all", "--objects"])
    if proc.returncode != 0:
        sys.stderr.write(proc.stderr or "")
        sys.exit(1)
    out: list[tuple[str, str]] = []
    for line in proc.stdout.splitlines():
        line = line.strip()
        if len(line) < 40:
            continue
        oid = line[:40]
        if len(oid) != 40:
            continue
        path_hint = line[41:].strip() if len(line) > 41 else ""
        out.append((oid, path_hint))
    return out


def cat_blob(repo: Path, oid: str) -> bytes:
    p = subprocess.run(
        ["git", "-C", str(repo), "cat-file", "blob", oid],
        capture_output=True,
    )
    return p.stdout if p.returncode == 0 else b""


def head_paths(repo: Path, branch: str) -> list[str]:
    proc = run(["git", "-C", str(repo), "ls-tree", "-r", "--name-only", branch])
    return [l.strip() for l in proc.stdout.splitlines() if l.strip()]


def emit_callback(oid_stubs_path: Path) -> str:
    """Body-only blob callback for git-filter-repo (loads oid-stubs.json at runtime)."""
    p = str(oid_stubs_path.resolve())
    # git-filter-repo wraps this as: def blob_callback(blob): <body>
    return f'''import json
from pathlib import Path
_relay_stubs = []
if not _relay_stubs:
    raw = json.loads(Path({p!r}).read_text(encoding="utf-8"))
    _relay_stubs.append({{
        oid: "relay-ipfs-v1\\n" + v["cid"] + "\\n" + v["mime"] + "\\nsha256=" + v["sha256"] + "\\n"
        for oid, v in raw.items()
    }})
oid = blob.original_id
if isinstance(oid, bytes):
    oid = oid.decode("ascii")
stub = _relay_stubs[0].get(oid)
if stub is not None:
    blob.data = stub.encode("utf-8")
else:
    data = blob.data
    if len(data) > 512 and data[:12] != b"relay-ipfs-v1":
        if b"\\0" in data[:8192]:
            raise SystemExit(f"unpinned binary blob {{oid}}")
'''


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("repo", type=Path)
    ap.add_argument("--workdir", type=Path, required=True)
    ap.add_argument("--branch", default="main")
    ap.add_argument("--max-blobs", type=int, default=0, help="0 = unlimited")
    ap.add_argument("--resume", action="store_true")
    args = ap.parse_args()

    repo = args.repo.resolve()
    work = args.workdir.resolve()
    work.mkdir(parents=True, exist_ok=True)

    oid_path = work / "oid-stubs.json"
    oid_stubs: dict[str, dict] = {}
    if args.resume and oid_path.is_file():
        oid_stubs = json.loads(oid_path.read_text(encoding="utf-8"))

    if run(["ipfs", "id"]).returncode != 0:
        sys.stderr.write("ipfs daemon not reachable; start with: ipfs daemon\n")
        return 1

    seen_oids: set[str] = set()
    blobs = iter_all_blobs(repo)
    print(f"scanning {len(blobs)} object lines from rev-list...", flush=True)
    pinned = 0
    skipped = 0

    for oid, path_hint in blobs:
        if oid in seen_oids:
            continue
        seen_oids.add(oid)
        if oid in oid_stubs:
            skipped += 1
            continue
        if args.max_blobs and pinned >= args.max_blobs:
            break
        data = cat_blob(repo, oid)
        if not data or not is_binary(path_hint, data):
            continue
        mime = guess_mime(path_hint or "application/octet-stream")
        sha = sha256_bytes(data)
        try:
            cid = ipfs_add(data, suffix=Path(path_hint).suffix if path_hint else ".bin")
        except RuntimeError as e:
            sys.stderr.write(f"ipfs add failed for {oid} {path_hint}: {e}\n")
            return 1
        oid_stubs[oid] = {
            "cid": cid,
            "mime": mime,
            "sha256": sha,
            "bytes": len(data),
            "path_hint": path_hint,
        }
        pinned += 1
        if pinned % 25 == 0:
            oid_path.write_text(json.dumps(oid_stubs, indent=2), encoding="utf-8")
            print(f"pinned {pinned} blobs (last {path_hint} -> {cid})", flush=True)

    oid_path.write_text(json.dumps(oid_stubs, indent=2), encoding="utf-8")
    print(f"oid-stubs: {len(oid_stubs)} binaries pinned ({pinned} new, {skipped} resumed)")

    rows = []
    assets = []
    for path in head_paths(repo, args.branch):
        p2 = run(["git", "-C", str(repo), "ls-tree", args.branch, path], text=True)
        if p2.returncode != 0:
            continue
        parts = p2.stdout.split()
        if len(parts) < 3:
            continue
        blob_oid = parts[2]
        stub = oid_stubs.get(blob_oid)
        if not stub:
            continue
        rows.append({
            "path": path,
            "oid": blob_oid,
            "cid": stub["cid"],
            "sha256": stub["sha256"],
            "bytes": stub["bytes"],
            "mime": stub["mime"],
        })
        assets.append({
            "id": path.replace("/", "_"),
            "path": path,
            "ipfs": stub["cid"],
            "contentType": stub["mime"],
            "sha256": stub["sha256"],
            "bytes": stub["bytes"],
        })

    csv_path = work / "head-paths.csv"
    with csv_path.open("w", newline="", encoding="utf-8") as f:
        w = csv.DictWriter(f, fieldnames=["path", "oid", "cid", "sha256", "bytes", "mime"])
        w.writeheader()
        w.writerows(rows)

    manifest = {"schema": SCHEMA, "assets": sorted(assets, key=lambda a: a["path"])}
    (work / MANIFEST).write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")

    cb = work / "blob-callback.py"
    cb.write_text(emit_callback(oid_path), encoding="utf-8")
    print(f"wrote {cb}, {csv_path}, {work / MANIFEST}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
