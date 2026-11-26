#!/usr/bin/env python3
"""Bake pre-rendered SSR fragments into a relay-ssr-cache worktree.

Publisher / fat node only — Nanodes must never run this queue.

Layout under --prefix (e.g. sites/paradigmthreat.net/):
  _shell.html          shared chrome with <!--RELAY_SSR_BODY-->
  _meta.json           contentTip + hookAssetsTip stamps
  index.body.html      /
  path/to/page.body.html

Modes:
  --full     re-bake every markdown path under content
  --delta    only paths changed since _meta.json contentTip (git diff)
  --enqueue  write a job file for relay-ssr-bake-queue.sh (no bake)

Production bake: CleverScript via `relay-ssr-bake` (QA before write).
Bootstrap only: `--simple` markdown HTML (never the publish gate).
Optional --http-origin fetches live HTML from a publisher for richer bodies.

Path parallelism: CleverScript holds a process-wide hook lock, so in-process
threads do not speed renders. This script fans out **multiple relay-ssr-bake
processes** (cap: RELAY_SSR_BAKE_PARALLEL, else RELAY_SSR_MAX_CONCURRENT, default 2).
"""

from __future__ import annotations

import argparse
import html
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import urllib.error
import urllib.request
from pathlib import Path

BODY_MARKER = "<!--RELAY_SSR_BODY-->"
# Keep in sync with relay-gateway/src/git/ssr_html_quality.rs FORBIDDEN_HARD
# and docs/examples/ssr-qa.markers.json forbid_always.
FORBIDDEN_HARD = (
    "relay-error-boundary",
    "data-error-boundary",
    "Runtime error",
    "Property 'origin' not found",
    "CleverScript hook panicked",
    "ssrClientHookFromGetContext disabled",
)


def run(cmd: list[str], cwd: Path | None = None) -> str:
    r = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, check=False)
    if r.returncode != 0:
        raise RuntimeError(f"cmd failed ({r.returncode}): {' '.join(cmd)}\n{r.stderr}")
    return r.stdout.strip()


def tip_oid(repo: Path, branch: str) -> str:
    return run(["git", "-C", str(repo), "rev-parse", f"refs/heads/{branch}"])


# Tip trees often carry local tooling; never bake these into ssr-cache.
_SKIP_DIR_PARTS = frozenset(
    {
        ".git",
        ".venv",
        ".venv-img",
        "venv",
        "node_modules",
        "__pycache__",
        ".mypy_cache",
        ".tox",
        ".pytest_cache",
        "dist",
        "build",
        ".turbo",
        ".next",
    }
)


def _is_git_repo(content: Path) -> bool:
    try:
        return (
            run(["git", "-C", str(content), "rev-parse", "--is-inside-work-tree"]).strip()
            in ("true", "false")
            or run(["git", "-C", str(content), "rev-parse", "--is-bare-repository"]).strip()
            == "true"
        )
    except RuntimeError:
        return (content / "HEAD").exists() and (content / "objects").is_dir()


def _git_ls_md(content: Path, rev: str) -> list[Path]:
    """List *.md paths at rev (works for bare + worktree). Returns Paths under content/."""
    try:
        out = run(
            ["git", "-C", str(content), "ls-tree", "-r", "--name-only", rev]
        )
    except RuntimeError:
        return []
    paths: list[Path] = []
    for line in out.splitlines():
        rel = line.strip()
        if not rel.endswith(".md"):
            continue
        parts = Path(rel).parts
        if any(part in _SKIP_DIR_PARTS for part in parts):
            continue
        paths.append(content / rel)
    return sorted(paths)


def list_md_files(content: Path, rev: str = "HEAD") -> list[Path]:
    # Publisher enqueue passes a bare repo; never rely on a worktree checkout.
    if _is_git_repo(content):
        return _git_ls_md(content, rev)
    out: list[Path] = []
    for p in content.rglob("*.md"):
        if any(part in _SKIP_DIR_PARTS for part in p.parts):
            continue
        out.append(p)
    return sorted(out)


def body_is_good(path: Path) -> bool:
    """True when an existing body should be kept (resume / no-clobber)."""
    if not path.is_file():
        return False
    try:
        text = path.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return False
    if len(text.strip()) < 32:
        return False
    for needle in FORBIDDEN_HARD:
        if needle in text:
            return False
    return True


def rel_body_name(md: Path, content_root: Path) -> str:
    rel = md.relative_to(content_root).as_posix()
    if rel.endswith(".md"):
        rel = rel[:-3]
    if rel in ("", "index"):
        return "index.body.html"
    return f"{rel}.body.html"


def md_to_simple_html(text: str, title: str) -> str:
    # Minimal offline body — crawlers get headings/paragraphs; client hydrates full UI.
    lines = text.splitlines()
    body_lines: list[str] = [f"<article data-relay-bake=\"simple\">", f"<h1>{html.escape(title)}</h1>"]
    para: list[str] = []

    def flush_para() -> None:
        nonlocal para
        if para:
            body_lines.append("<p>" + html.escape(" ".join(para)) + "</p>")
            para = []

    for line in lines:
        if line.startswith("#"):
            flush_para()
            m = re.match(r"^(#{1,6})\s+(.*)", line)
            if m:
                level = len(m.group(1))
                body_lines.append(f"<h{level}>{html.escape(m.group(2).strip())}</h{level}>")
            continue
        if not line.strip():
            flush_para()
            continue
        para.append(line.strip())
    flush_para()
    body_lines.append("</article>")
    return "\n".join(body_lines) + "\n"


def default_shell(site_name: str) -> str:
    return f"""<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{html.escape(site_name)}</title>
</head>
<body data-relay-ssr-cache="1">
<main id="relay-root">
{BODY_MARKER}
</main>
<script type="module">
/* Client hooks own navigation / open-closed UI after hydrate. */
</script>
</body>
</html>
"""


def changed_md_since(content: Path, old_tip: str, new_tip: str) -> list[Path] | None:
    """Return changed *.md Paths, [] if nothing changed, or None to force full."""
    if not old_tip:
        return None  # no prior tip → full
    if old_tip == new_tip:
        return []  # already baked this tip
    try:
        out = run(
            [
                "git",
                "-C",
                str(content),
                "diff",
                "--name-only",
                "--diff-filter=ACMR",
                f"{old_tip}..{new_tip}",
                "--",
                "*.md",
            ]
        )
    except RuntimeError:
        return None
    paths: list[Path] = []
    for line in out.splitlines():
        rel = line.strip()
        if not rel.endswith(".md"):
            continue
        parts = Path(rel).parts
        if any(part in _SKIP_DIR_PARTS for part in parts):
            continue
        # Synthetic path under content root (bare has no blob checkout).
        paths.append(content / rel)
    return paths


def fetch_http_body(origin: str, path: str) -> str | None:
    url = origin.rstrip("/") + "/" + path.lstrip("/")
    req = urllib.request.Request(url, headers={"Accept": "text/html"})
    try:
        with urllib.request.urlopen(req, timeout=60) as resp:
            raw = resp.read().decode("utf-8", errors="replace")
    except (urllib.error.URLError, TimeoutError, ValueError):
        return None
    # Prefer <main>…</main> inner HTML when present.
    m = re.search(r"<main[^>]*>(.*)</main>", raw, re.I | re.S)
    if m:
        return m.group(1).strip() + "\n"
    return f'<article data-relay-bake="http">{raw}</article>\n'


def write_meta(prefix_dir: Path, content_tip: str, hook_tip: str, mode: str) -> None:
    meta = {
        "schema": "relay.ssr-cache-meta.v1",
        "contentTip": content_tip,
        "hookAssetsTip": hook_tip,
        "bakeMode": mode,
    }
    (prefix_dir / "_meta.json").write_text(json.dumps(meta, indent=2) + "\n", encoding="utf-8")


def qa_body_or_die(body: str, path: str) -> None:
    if not body.strip():
        raise RuntimeError(f"QA fail {path}: empty body")
    for needle in FORBIDDEN_HARD:
        if needle in body:
            raise RuntimeError(f"QA fail {path}: forbidden marker {needle!r}")


def find_ssr_bake_bin() -> str | None:
    env = os.environ.get("RELAY_SSR_BAKE_BIN", "").strip()
    if env and Path(env).is_file():
        return env
    which = shutil.which("relay-ssr-bake")
    if which:
        return which
    # Dev tree next to this script
    cand = Path(__file__).resolve().parents[1] / "target" / "release" / "relay-ssr-bake"
    if cand.is_file():
        return str(cand)
    cand = Path(__file__).resolve().parents[1] / "target" / "debug" / "relay-ssr-bake"
    if cand.is_file():
        return str(cand)
    return None


def bake_parallel_workers() -> int:
    """Process count for path-level CleverScript bake (not Nanode live SSR)."""
    raw = os.environ.get("RELAY_SSR_BAKE_PARALLEL", "").strip()
    if not raw:
        raw = os.environ.get("RELAY_SSR_MAX_CONCURRENT", "2").strip()
    try:
        n = int(raw)
    except ValueError:
        n = 2
    return max(1, min(n, 16))


def _chunk_size() -> int:
    """Paths per worker process before respawn (limits RSS growth / oomd risk)."""
    raw = os.environ.get("RELAY_SSR_BAKE_CHUNK", "").strip()
    try:
        n = int(raw) if raw else 40
    except ValueError:
        n = 40
    return max(1, min(n, 200))


def _run_ssr_bake_chunk(
    bin_path: str,
    content: Path,
    branch: str,
    chunk: list[str],
    out_dir: Path,
    log_path: Path | None = None,
) -> tuple[int, str]:
    """Run one bake process. Never signals siblings.

    Returns (returncode, stderr_tail). rc -15/-9 usually means external SIGTERM/KILL
    (e.g. systemd-oomd), not an in-process QA fail (those are rc 1 with bodies kept).
    """
    out_dir.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile("w", suffix=".txt", delete=False) as tf:
        for p in chunk:
            tf.write(p.rstrip() + "\n")
        paths_file = tf.name
    log_f = None
    try:
        # Log to a file — capture_output=True keeps multi-MB stderr in RAM per worker
        # and has historically coincided with process-group SIGTERM under pressure.
        if log_path is None:
            log_path = out_dir / "_bake.log"
        log_f = open(log_path, "wb")
        proc = subprocess.Popen(
            [
                bin_path,
                "--repo",
                str(content),
                "--rev",
                branch,
                "--paths-file",
                paths_file,
                "--out-dir",
                str(out_dir),
            ],
            stdout=log_f,
            stderr=subprocess.STDOUT,
            start_new_session=True,  # own PGID: sibling/parent signals don't fan out
        )
        rc = proc.wait()
        tail = ""
        try:
            raw = log_path.read_bytes()
            tail = raw[-4000:].decode("utf-8", errors="replace")
        except OSError:
            pass
        return rc, tail
    finally:
        if log_f is not None:
            log_f.close()
        Path(paths_file).unlink(missing_ok=True)


def _merge_shard_bodies(shard: Path, out_dir: Path, *, clobber: bool) -> int:
    n = 0
    if not shard.is_dir():
        return 0
    for src in shard.rglob("*.body.html"):
        rel = src.relative_to(shard)
        dest = out_dir / rel
        if not clobber and body_is_good(dest):
            continue
        dest.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(src, dest)
        n += 1
    return n


def clever_bake_bodies(
    content: Path,
    branch: str,
    request_paths: list[str],
    out_dir: Path,
) -> None:
    bin_path = find_ssr_bake_bin()
    if not bin_path:
        raise RuntimeError(
            "relay-ssr-bake binary not found; build it or set RELAY_SSR_BAKE_BIN "
            "(or pass --simple for bootstrap-only markdown)"
        )
    if not request_paths:
        return

    out_dir.mkdir(parents=True, exist_ok=True)
    workers = bake_parallel_workers()
    chunk_size = _chunk_size()

    # Split into small batches so a killed worker loses at most chunk_size paths,
    # then fan batches across up to `workers` processes. Never terminate siblings
    # on first failure — wait for every Popen.
    batches: list[list[str]] = []
    for i in range(0, len(request_paths), chunk_size):
        batches.append(request_paths[i : i + chunk_size])

    errors: list[str] = []
    salvaged = 0
    with tempfile.TemporaryDirectory(prefix="ssr-bake-par-") as shard_root:
        shard_root_path = Path(shard_root)
        # Process batches in waves of `workers`
        for wave_start in range(0, len(batches), max(1, workers)):
            wave = batches[wave_start : wave_start + max(1, workers)]
            pops: list[tuple[subprocess.Popen[bytes], Path, Path, list[str]]] = []
            path_files: list[Path] = []
            for wi, batch in enumerate(wave):
                shard = shard_root_path / f"w{wave_start + wi}"
                shard.mkdir(parents=True, exist_ok=True)
                pf = shard / "paths.txt"
                pf.write_text("".join(p.rstrip() + "\n" for p in batch), encoding="utf-8")
                path_files.append(pf)
                log_path = shard / "_bake.log"
                log_f = open(log_path, "wb")
                proc = subprocess.Popen(
                    [
                        bin_path,
                        "--repo",
                        str(content),
                        "--rev",
                        branch,
                        "--paths-file",
                        str(pf),
                        "--out-dir",
                        str(shard),
                    ],
                    stdout=log_f,
                    stderr=subprocess.STDOUT,
                    start_new_session=True,
                )
                # stash log handle on proc for close after wait
                proc._bake_log_f = log_f  # type: ignore[attr-defined]
                pops.append((proc, shard, log_path, batch))

            # Wait for ALL workers in the wave — do not kill on first failure.
            for proc, shard, log_path, batch in pops:
                rc = proc.wait()
                try:
                    proc._bake_log_f.close()  # type: ignore[attr-defined]
                except Exception:
                    pass
                # Always clobber: shards are fresh bakes for this request set.
                # (Resume skipping happens earlier by omitting paths from md_to_bake.)
                salvaged += _merge_shard_bodies(shard, out_dir, clobber=True)
                if rc != 0:
                    try:
                        tail = log_path.read_bytes()[-2000:].decode(
                            "utf-8", errors="replace"
                        )
                    except OSError:
                        tail = ""
                    errors.append(
                        f"wave worker rc={rc} paths={len(batch)} "
                        f"first={batch[0] if batch else '?'} "
                        f"last={batch[-1] if batch else '?'}\n{tail}"
                    )
                    if rc in (-15, -9, 143, 137):
                        print(
                            f"relay-ssr-bake: worker signalled rc={rc} "
                            f"(SIGTERM/SIGKILL — often systemd-oomd); "
                            f"continuing remaining waves",
                            file=sys.stderr,
                            flush=True,
                        )

            print(
                f"clever bake wave={wave_start // max(1, workers)} "
                f"workers={len(wave)} paths_done~={min(len(request_paths), (wave_start + len(wave)) * chunk_size)} "
                f"bodies_out={salvaged} errors={len(errors)}",
                flush=True,
            )

    print(
        f"clever bake parallel workers={workers} chunk={chunk_size} "
        f"paths={len(request_paths)} bodies={salvaged} chunk_errors={len(errors)}",
        flush=True,
    )
    if errors:
        raise RuntimeError(
            "parallel relay-ssr-bake had worker failures "
            f"(salvaged/merged {salvaged} bodies):\n" + "\n---\n".join(errors[:8])
        )


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--content", required=True, type=Path, help="Content git checkout or bare")
    ap.add_argument("--cache-worktree", required=True, type=Path, help="relay-ssr-cache worktree")
    ap.add_argument("--prefix", required=True, help="e.g. sites/paradigmthreat.net/")
    ap.add_argument("--site-name", default="", help="Shell <title>")
    ap.add_argument("--content-branch", default="main")
    ap.add_argument("--hook-assets", type=Path, default=None)
    ap.add_argument("--hook-assets-branch", default="release")
    ap.add_argument("--full", action="store_true")
    ap.add_argument("--delta", action="store_true")
    ap.add_argument("--enqueue", type=Path, help="Write job JSON instead of baking")
    ap.add_argument("--http-origin", default="", help="Optional publisher origin for richer bodies")
    ap.add_argument(
        "--simple",
        action="store_true",
        help="Bootstrap-only markdown HTML (skip CleverScript). Not for production park.",
    )
    ap.add_argument("--commit", action="store_true", help="git add+commit in cache worktree")
    args = ap.parse_args()

    prefix = args.prefix.strip().strip("/") + "/"
    site = args.site_name or prefix.strip("/").split("/")[-1]
    content = args.content.resolve()
    cache = args.cache_worktree.resolve()

    content_tip = tip_oid(content, args.content_branch)
    hook_tip = ""
    if args.hook_assets and args.hook_assets.exists():
        hook_tip = tip_oid(args.hook_assets.resolve(), args.hook_assets_branch)

    if args.enqueue:
        job = {
            "content": str(content),
            "cacheWorktree": str(cache),
            "prefix": prefix,
            "siteName": site,
            "contentBranch": args.content_branch,
            "contentTip": content_tip,
            "hookAssetsTip": hook_tip,
            "full": bool(args.full) or not args.delta,
            "httpOrigin": args.http_origin,
        }
        args.enqueue.parent.mkdir(parents=True, exist_ok=True)
        args.enqueue.write_text(json.dumps(job, indent=2) + "\n", encoding="utf-8")
        print(f"enqueued {args.enqueue}")
        return 0

    prefix_dir = cache / prefix
    prefix_dir.mkdir(parents=True, exist_ok=True)

    meta_path = prefix_dir / "_meta.json"
    old_content = ""
    old_hook = ""
    if meta_path.is_file():
        try:
            old = json.loads(meta_path.read_text(encoding="utf-8"))
            old_content = old.get("contentTip") or ""
            old_hook = old.get("hookAssetsTip") or ""
        except json.JSONDecodeError:
            pass

    force_full = args.full or (hook_tip and hook_tip != old_hook) or not args.delta
    if force_full:
        mode = "full"
        md_files = list_md_files(content, args.content_branch)
    else:
        mode = "delta"
        changed = changed_md_since(content, old_content, content_tip)
        if changed is None:
            mode = "full"
            md_files = list_md_files(content, args.content_branch)
        else:
            md_files = changed

    shell_path = prefix_dir / "_shell.html"
    # Never clobber a good hydrate-capable shell with the stub default.
    if not shell_path.is_file() or not body_is_good(shell_path):
        shell_path.write_text(default_shell(site), encoding="utf-8")
        qa_body_or_die(shell_path.read_text(encoding="utf-8"), "_shell.html")

    # Full bake resume: skip paths that already have a good body.
    # Delta always rebakes changed paths (content tip moved).
    skipped = 0
    md_to_bake: list[Path] = []
    for md in md_files:
        body_name = rel_body_name(md, content)
        dest = prefix_dir / body_name
        if mode == "full" and body_is_good(dest):
            skipped += 1
            continue
        md_to_bake.append(md)

    request_paths: list[str] = []
    for md in md_to_bake:
        rel = md.relative_to(content).as_posix()
        if rel.endswith(".md"):
            rel = rel[:-3]
        if rel in ("", "index"):
            request_paths.append("/")
        else:
            request_paths.append("/" + rel)
    # Park homes often have no index.md (client hook owns `/`); still bake `/`.
    index_dest = prefix_dir / "index.body.html"
    if "/" not in request_paths and not body_is_good(index_dest):
        request_paths.insert(0, "/")

    if skipped:
        print(
            f"resume skip good bodies={skipped} remaining={len(request_paths)}",
            flush=True,
        )

    written = 0
    missing: list[str] = []
    bake_err: str | None = None
    use_clever = not args.simple and not args.http_origin
    if use_clever:
        if not request_paths:
            write_meta(prefix_dir, content_tip, hook_tip, mode)
            print(
                f"baked mode={mode} engine=clever files=0 missing=0 skipped={skipped} "
                f"contentTip={content_tip[:12]} prefix={prefix}"
            )
            return 0
        # Bake straight into the cache prefix so mid-run kills keep completed
        # bodies (full-mode resume omits good paths from request_paths above).
        try:
            clever_bake_bodies(
                content, args.content_branch, request_paths, prefix_dir
            )
        except Exception as e:
            bake_err = str(e)
            print(f"relay-ssr-bake: partial bake: {e}", file=sys.stderr)

        def _body_name_for_request(path: str) -> str:
            p = path.strip().strip("/")
            if not p or p == "index":
                return "index.body.html"
            if p.endswith(".md"):
                p = p[:-3]
            return f"{p}.body.html"

        for req in request_paths:
            body_name = _body_name_for_request(req)
            dest = prefix_dir / body_name
            # Intentional bake targets always count as written when good so
            # --commit runs for delta overwrites of existing bodies.
            if body_is_good(dest):
                written += 1
            else:
                missing.append(body_name)
    else:
        for md in md_files:
            body_name = rel_body_name(md, content)
            out = prefix_dir / body_name
            out.parent.mkdir(parents=True, exist_ok=True)
            text = md.read_text(encoding="utf-8", errors="replace")
            title = md.stem.replace("-", " ").title()
            if args.http_origin:
                url_path = body_name.replace(".body.html", "")
                if url_path == "index":
                    url_path = ""
                body = fetch_http_body(args.http_origin, url_path) or md_to_simple_html(
                    text, title
                )
            else:
                body = md_to_simple_html(text, title)
            qa_body_or_die(body, body_name)
            out.write_text(body, encoding="utf-8")
            written += 1

    if written or skipped:
        write_meta(prefix_dir, content_tip, hook_tip, mode)
    print(
        f"baked mode={mode} engine={'clever' if use_clever else 'simple/http'} "
        f"files={written} missing={len(missing)} skipped={skipped} "
        f"contentTip={content_tip[:12]} prefix={prefix}"
    )
    if missing:
        preview = ", ".join(missing[:12])
        more = f" (+{len(missing) - 12} more)" if len(missing) > 12 else ""
        print(f"missing bodies: {preview}{more}", file=sys.stderr)

    if args.commit and written and not missing and not bake_err:
        run(["git", "-C", str(cache), "add", "-A", prefix.rstrip("/")])
        msg = f"ssr-cache bake {site} {mode} {content_tip[:12]}"
        st = subprocess.run(
            ["git", "-C", str(cache), "diff", "--cached", "--quiet"],
            check=False,
        )
        if st.returncode != 0:
            run(["git", "-C", str(cache), "commit", "-m", msg])
            print(f"committed: {msg}")
        else:
            print("no cache changes to commit")
    if bake_err or missing:
        return 1
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception as e:
        print(f"relay-ssr-bake: {e}", file=sys.stderr)
        raise SystemExit(1)
