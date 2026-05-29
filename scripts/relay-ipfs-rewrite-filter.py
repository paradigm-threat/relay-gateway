#!/usr/bin/env python3
"""
git-filter-repo blob callback: replace binaries with relay-ipfs-v1 pointer stubs.

Requires prior relay-ipfs-migrate-inventory CSV:

  python3 relay-ipfs-rewrite-filter.py --csv ipfs-migration.csv > /tmp/filter-blobs.py
  git filter-repo --force --blob-callback "$(cat /tmp/filter-blobs.py)"

Or run this script's --print-callback only.
"""
from __future__ import annotations

import argparse
import csv
from pathlib import Path

POINTER_MAGIC = "relay-ipfs-v1"


def load_csv(path: Path) -> dict[str, dict]:
    by_path: dict[str, dict] = {}
    with path.open(encoding="utf-8") as f:
        for row in csv.DictReader(f):
            by_path[row["path"]] = row
    return by_path


def stub_for_row(row: dict) -> str:
    return f"{POINTER_MAGIC}\n{row['cid']}\n{row['mime']}\nsha256={row['sha256']}\n"


def emit_callback(by_path: dict[str, dict]) -> str:
    stubs = {p: stub_for_row(r) for p, r in by_path.items()}
  return f'''def blob_callback(blob, meta):
    import hashlib
    STUBS = {repr(stubs)}
    path = meta.filename.decode("utf-8", "surrogateescape")
    if path in STUBS:
        blob.data = STUBS[path].encode("utf-8")
        return
    data = blob.data
    if len(data) > 256 and b"\\0" in data[:4096]:
        raise SystemExit(f"binary blob not in migration CSV: {{path}}")
'''


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--csv", type=Path, required=True)
    ap.add_argument("-o", "--output", type=Path, help="Write callback to file")
    args = ap.parse_args()
    by_path = load_csv(args.csv)
    code = emit_callback(by_path)
    if args.output:
        args.output.write_text(code, encoding="utf-8")
        print(f"wrote {args.output}")
    else:
        print(code)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
