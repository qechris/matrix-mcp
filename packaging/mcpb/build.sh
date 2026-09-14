#!/usr/bin/env bash
#
# Builds the matrix-mcp Desktop Extension (.mcpb) bundle -- one package
# covering macOS and Linux (arch-detected at launch) -- from already-built
# release binaries.
#
# Requirements on the host: node (for the manifest generator and the mcpb
# CLI, run via npx).
#
# Usage: build.sh VERSION BIN_DIR OUT_DIR
#   VERSION  e.g. 0.2.0 (no leading "v")
#   BIN_DIR  directory containing exactly these executable binaries:
#              matrix-mcp-darwin-arm64  matrix-mcp-darwin-x64
#              matrix-mcp-linux-arm64   matrix-mcp-linux-x64
#   OUT_DIR  where the resulting matrix-mcp-VERSION.mcpb is written
set -euo pipefail

VERSION="${1:?usage: build.sh VERSION BIN_DIR OUT_DIR}"
BIN_DIR="${2:?usage: build.sh VERSION BIN_DIR OUT_DIR}"
OUT_DIR="${3:?usage: build.sh VERSION BIN_DIR OUT_DIR}"

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
MCPB="npx --yes @anthropic-ai/mcpb@2.1.2"

for id in darwin-arm64 darwin-x64 linux-arm64 linux-x64; do
  bin="$BIN_DIR/matrix-mcp-$id"
  [ -x "$bin" ] || { echo "::error::missing or non-executable binary: $bin" >&2; exit 1; }
done

mkdir -p "$OUT_DIR"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

stage="$WORK/universal"
mkdir -p "$stage/bin"
for id in darwin-arm64 darwin-x64 linux-arm64 linux-x64; do
  cp "$BIN_DIR/matrix-mcp-$id" "$stage/bin/matrix-mcp-$id"
  chmod +x "$stage/bin/matrix-mcp-$id"
done
cp "$HERE/bin/launch-darwin.sh" "$HERE/bin/launch-linux.sh" "$stage/bin/"
chmod +x "$stage/bin/launch-darwin.sh" "$stage/bin/launch-linux.sh"
cp "$ROOT/LICENSE" "$stage/LICENSE"
node "$HERE/generate-manifest.mjs" --version "$VERSION" --out "$stage/manifest.json"
$MCPB validate "$stage/manifest.json"
$MCPB pack "$stage" "$OUT_DIR/matrix-mcp-$VERSION.mcpb"

echo "== done =="
ls -la "$OUT_DIR"
