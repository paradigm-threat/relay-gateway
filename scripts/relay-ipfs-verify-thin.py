#!/usr/bin/env python3
"""Audit thin bare repo history for leftover binaries and pin coverage."""
from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

POINTER = b"relay-ipfs-v1\n"
MIN_BINARY = 512


def run(cmd: list[str]) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, check=False, capture_output=True, text=True)


def is_likely_binary(data: bytes) -> bool:
    if not data or data.startswith(POINTER):
        return False
    if b"\0" in data[:8192]:
        return True
    try:
        data.decode("utf-8")
        return False
    except UnicodeDecodeError:
        return len(data) > MIN_BINARY


def main() -> int:
    bare = Path(sys.argv[1]).resolve()
    stubs_path = Path(sys.argv[2]).resolve() if len(sys.argv) > 2 else None

    proc = run(["git", "-C", str(bare), "rev-list", "--all", "--objects"])
    if proc.returncode != 0:
        sys.stderr.write(proc.stderr or "")
        return 1

    seen: set[str] = set()
    offenders: list[tuple[str, int, str]] = []
    for line in proc.stdout.splitlines():
        line = line.strip()
        if len(line) < 40:
            continue
        oid = line[:40]
        if oid in seen:
            continue
        seen.add(oid)
        cat = subprocess.run(
            ["git", "-C", str(bare), "cat-file", "blob", oid],
            capture_output=True,
        )
        if cat.returncode != 0:
            continue
        data = cat.stdout
        if len(data) <= MIN_BINARY:
            continue
        if data.startswith(POINTER):
            continue
        if is_likely_binary(data):
            hint = line[41:].strip() if len(line) > 41 else ""
            offenders.append((oid, len(data), hint))

    print(f"unique blobs scanned: {len(seen)}")
    if offenders:
        print(f"FAIL: {len(offenders)} unpinned binary blob(s) in history:", file=sys.stderr)
        for oid, size, hint in offenders[:20]:
            print(f"  {oid} {size} bytes {hint}", file=sys.stderr)
        if len(offenders) > 20:
            print(f"  ... and {len(offenders) - 20} more", file=sys.stderr)
        return 1
    print("OK: no unpinned binary blobs in history")

    large = subprocess.run(
        [
            "git",
            "-C",
            str(bare),
            "rev-list",
            "--objects",
            "--all",
        ],
        capture_output=True,
        text=True,
    )
    batch = subprocess.run(
        ["git", "-C", str(bare), "cat-file", "--batch-check=%(objecttype) %(objectname) %(objectsize)"],
        input=large.stdout,
        capture_output=True,
        text=True,
    )
    big = [ln for ln in batch.stdout.splitlines() if ln.startswith("blob ") and int(ln.split()[2]) > 262144]
    if big:
        print(f"FAIL: {len(big)} blob(s) >256KiB", file=sys.stderr)
        for ln in big[:10]:
            print(f"  {ln}", file=sys.stderr)
        return 1
    print("OK: no blobs >256KiB")

    if stubs_path and stubs_path.is_file():
        stubs = json.loads(stubs_path.read_text(encoding="utf-8"))
        cids = sorted({v["cid"] for v in stubs.values()})
        missing = []
        for cid in cids:
            r = subprocess.run(["ipfs", "pin", "ls", cid], capture_output=True, text=True)
            if r.returncode != 0 or cid not in r.stdout:
                missing.append(cid)
        if missing:
            print(f"FAIL: {len(missing)} CIDs not pinned (first {missing[0]})", file=sys.stderr)
            return 1
        print(f"OK: all {len(cids)} CIDs pinned locally")

    return 0


if __name__ == "__main__":
    sys.exit(main())
