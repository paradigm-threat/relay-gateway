#!/usr/bin/env bash
# Build md2ast WASM for @clevertree/relay-markdown (markdown-only hosts).
# relay-client-web continues to use relay-runtime WASM (CleverScript + md2ast).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
PKG="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${PKG}/wasm"
MD2AST_LOCAL="${MD2AST:-${ROOT}/../../md2ast}"
MD2AST_NPM="${PKG}/node_modules/@clevertree/md2ast"

copy_from_npm() {
  local src="$1"
  mkdir -p "$OUT"
  find "$OUT" -mindepth 1 -maxdepth 1 -exec rm -rf {} +
  cp "${src}/md2ast.js" "${src}/md2ast_bg.wasm" "${src}/md2ast.d.ts" "$OUT/"
  echo "md2ast WASM copied from ${src} -> $OUT"
}

if [[ -f "${MD2AST_NPM}/md2ast.js" && -f "${MD2AST_NPM}/md2ast_bg.wasm" ]]; then
  copy_from_npm "$MD2AST_NPM"
  exit 0
fi

if [[ -f "${MD2AST_LOCAL}/Cargo.toml" ]]; then
  command -v wasm-pack >/dev/null || {
    echo "Install wasm-pack: https://rustwasm.github.io/wasm-pack/installer/"
    exit 1
  }
  mkdir -p "$OUT"
  find "$OUT" -mindepth 1 -maxdepth 1 -exec rm -rf {} +
  cd "$MD2AST_LOCAL"
  wasm-pack build --target web --out-dir "$OUT" --features wasm
  echo "md2ast WASM built from ${MD2AST_LOCAL} -> $OUT"
  exit 0
fi

echo "md2ast not found: install @clevertree/md2ast or set MD2AST= to local crate path"
exit 1
