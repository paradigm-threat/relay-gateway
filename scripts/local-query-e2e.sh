#!/usr/bin/env bash
# Local QUERY hardening smoke against a PT-shaped bare fixture.
# Usage: ./scripts/local-query-e2e.sh
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

echo "== cargo test (QUERY / MCP) =="
cargo test --lib 2>&1 | tee /tmp/relay-query-e2e-cargo.log | tail -20
grep -q 'test result: ok' /tmp/relay-query-e2e-cargo.log

FIX="$(mktemp -d /tmp/relay-query-e2e-XXXXXX)"
python3 - <<PY
import json, subprocess
from pathlib import Path
fix = Path("$FIX")
bare = fix / "pt-files.git"
work = fix / "work"
subprocess.check_call(["git", "init", "--bare", str(bare)], stdout=subprocess.DEVNULL)
work.mkdir()
subprocess.check_call(["git", "clone", str(bare), str(work)], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
src = Path("/home/ari/dev/pt/paradigm-threat-files/index.json")
full = json.load(src.open()) if src.exists() else {"blog":{"a.md":{"_title":"Portal","_keywords":["portal"]}}}
sample = {k: full[k] for k in ("blog", "cosmos", "events") if k in full} or full
(work / "index.json").write_text(json.dumps(sample))
(work / "hooks" / "server").mkdir(parents=True)
(work / "hooks" / "server" / "index.cs").write_text("export default function run(ctx) { return null; }\n")
(work / "hooks" / "server" / "query.cs").write_text(
    'export default function run(ctx) { return { plugin: "sqlite", collection: ctx.collection, query: ctx.query }; }\n'
)
(work / ".relay.yaml").write_text(
    "server:\n  hooks:\n    index: { path: hooks/server/index.cs }\n    query: { path: hooks/server/query.cs }\n  search:\n    plugin: sqlite\n"
)
subprocess.check_call(["git", "-C", str(work), "config", "user.email", "e2e@relay"])
subprocess.check_call(["git", "-C", str(work), "config", "user.name", "e2e"])
subprocess.check_call(["git", "-C", str(work), "add", "-A"])
subprocess.check_call(["git", "-C", str(work), "commit", "-m", "e2e"], stdout=subprocess.DEVNULL)
subprocess.check_call(["git", "-C", str(work), "push", "origin", "HEAD:main"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
print(bare)
PY

BARE="$FIX/pt-files.git"
echo "== external bare QUERY ($BARE) =="
RELAY_QUERY_E2E_BARE="$BARE" cargo test --lib optional_external_bare_query_e2e -- --nocapture
echo "OK fixture kept at $FIX"
