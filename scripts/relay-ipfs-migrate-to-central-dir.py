#!/usr/bin/env python3
"""
Rewrite thin-repo history: move inline relay-ipfs-v1 stubs to .relay/assets/{path}.ipfs
and remove the public-path tree entries (logical URLs served via hooks + manifest).

  relay-ipfs-migrate-to-central-dir.py /path/to/bare.git [--dry-run]

Requires: git. Run relay-ipfs-verify-thin.sh after.
"""
from __future__ import annotations

import argparse
import subprocess
import sys
from pathlib import Path

POINTER_MAGIC = b"relay-ipfs-v1\n"
ASSETS_PREFIX = ".relay/assets/"
IPFS_SUFFIX = ".ipfs"

BINARY_EXT = {
    ".png", ".jpg", ".jpeg", ".gif", ".webp", ".ico", ".pdf", ".zip",
    ".mp3", ".mp4", ".webm", ".mkv", ".avi", ".mov",
    ".woff", ".woff2", ".ttf", ".eot", ".doc", ".docx", ".epub",
}


def run(cmd: list[str], *, input_text: str | None = None, binary: bool = False) -> subprocess.CompletedProcess:
    kw: dict = {"capture_output": True}
    if binary:
        kw["text"] = False
    else:
        kw["text"] = True
        kw["encoding"] = "utf-8"
        kw["errors"] = "surrogateescape"
    if input_text is not None:
        kw["input"] = input_text
    return subprocess.run(cmd, check=False, **kw)


def is_asset_path(path: str) -> bool:
    if path.startswith(ASSETS_PREFIX):
        return False
    return Path(path).suffix.lower() in BINARY_EXT


def cat_blob(repo: Path, oid: str) -> bytes:
    p = run(["git", "-C", str(repo), "cat-file", "-p", oid], binary=True)
    if p.returncode != 0:
        return b""
    return p.stdout  # type: ignore[return-value]


def is_inline_pointer(repo: Path, oid: str) -> bool:
    return cat_blob(repo, oid)[:16].startswith(POINTER_MAGIC)


def mktree(repo: Path, lines: list[str]) -> str:
    body = "\n".join(lines)
    if body:
        body += "\n"
    proc = run(["git", "-C", str(repo), "mktree"], input_text=body)
    if proc.returncode != 0:
        raise RuntimeError(proc.stderr or "mktree failed")
    return proc.stdout.strip()


def _insert_path(root: dict, parts: list[str], mode: str, typ: str, sha: str) -> None:
    if len(parts) == 1:
        root[parts[0]] = (mode, typ, sha)
        return
    sub = root.setdefault(parts[0], {})
    if not isinstance(sub, dict):
        raise RuntimeError(f"path conflict at {parts[0]!r}")
    _insert_path(sub, parts[1:], mode, typ, sha)


def _tree_from_dict(repo: Path, node: dict) -> str:
    lines: list[str] = []
    for name in sorted(node.keys()):
        val = node[name]
        if isinstance(val, dict):
            sub_sha = _tree_from_dict(repo, val)
            lines.append(f"040000 tree {sub_sha}\t{name}")
        else:
            mode, typ, sha = val
            lines.append(f"{mode} {typ} {sha}\t{name}")
    return mktree(repo, lines)


def rewrite_tree(repo: Path, tree_oid: str) -> str:
    """Rewrite one commit tree.

    Must use `ls-tree -r` with full paths. A recursive per-subtree walk only sees
    basenames, which incorrectly nests `.relay/assets/` under each directory
    (e.g. `book/.relay/assets/foo.png.ipfs`) instead of repo-root
    `.relay/assets/book/foo.png.ipfs`.
    """
    proc = run(["git", "-C", str(repo), "ls-tree", "-r", tree_oid])
    if proc.returncode != 0:
        raise RuntimeError(proc.stderr or f"ls-tree {tree_oid}")
    root: dict = {}

    for line in proc.stdout.splitlines():
        if not line.strip():
            continue
        mode, typ, sha, name = line.split(maxsplit=3)
        if name.startswith('"') and name.endswith('"'):
            name = bytes(name[1:-1], "utf-8").decode("unicode_escape")
        if typ != "blob":
            # rare: submodule / gitlink — keep at path
            _insert_path(root, name.split("/"), mode, typ, sha)
            continue
        if is_asset_path(name) and is_inline_pointer(repo, sha):
            central = f"{ASSETS_PREFIX}{name}{IPFS_SUFFIX}"
            _insert_path(root, central.split("/"), mode, "blob", sha)
            continue
        _insert_path(root, name.split("/"), mode, "blob", sha)

    return _tree_from_dict(repo, root)


def rewrite_repo(repo: Path, dry_run: bool) -> None:
    proc = run(["git", "-C", str(repo), "rev-list", "--topo-order", "--reverse", "--all"])
    if proc.returncode != 0:
        sys.exit(proc.stderr or "rev-list failed")
    commits = [c.strip() for c in proc.stdout.splitlines() if c.strip()]
    remap: dict[str, str] = {}

    for old in commits:
        tp = run(["git", "-C", str(repo), "log", "-1", "--format=%T", old])
        tree_oid = tp.stdout.strip()
        pp = run(["git", "-C", str(repo), "log", "-1", "--format=%P", old])
        parents = [p for p in pp.stdout.split() if p]
        mp = run(["git", "-C", str(repo), "log", "-1", "--format=%B", old])
        message = mp.stdout

        new_tree = rewrite_tree(repo, tree_oid)
        new_parents = [remap.get(p, p) for p in parents]

        if dry_run:
            print(f"would rewrite {old[:8]} tree -> {new_tree[:8]}")
            remap[old] = old
            continue

        cmd = ["git", "-C", str(repo), "commit-tree", new_tree]
        for p in new_parents:
            cmd.extend(["-p", p])
        ct = run(cmd, input_text=message)
        if ct.returncode != 0:
            sys.stderr.write(ct.stderr or "commit-tree failed\n")
            sys.exit(1)
        remap[old] = ct.stdout.strip()

    if dry_run:
        print(f"dry-run: {len(commits)} commits")
        return

    ref_proc = run(
        ["git", "-C", str(repo), "for-each-ref", "--format=%(refname)", "refs/heads/", "refs/tags/"]
    )
    if ref_proc.returncode == 0:
        for ref in ref_proc.stdout.splitlines():
            ref = ref.strip()
            if not ref:
                continue
            old = run(["git", "-C", str(repo), "rev-parse", ref]).stdout.strip()
            if old in remap:
                run(["git", "-C", str(repo), "update-ref", ref, remap[old], old])

    print(f"rewrote {len(commits)} commits")


def main() -> int:
    ap = argparse.ArgumentParser(description="Move inline IPFS stubs to .relay/assets/")
    ap.add_argument("repo", type=Path, help="Bare git repository")
    ap.add_argument("--dry-run", action="store_true")
    args = ap.parse_args()
    repo = args.repo.resolve()
    if not (repo / "HEAD").exists():
        sys.stderr.write("not a bare repo?\n")
        return 1
    rewrite_repo(repo, args.dry_run)
    return 0


if __name__ == "__main__":
    sys.exit(main())
