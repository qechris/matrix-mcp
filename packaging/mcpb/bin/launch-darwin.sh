#!/bin/sh
# The mcpb manifest format only supports per-OS platform_overrides, not
# per-architecture, so this picks the right macOS binary at launch time.
set -e
dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
case "$(uname -m)" in
  arm64) exec "$dir/matrix-mcp-darwin-arm64" "$@" ;;
  x86_64) exec "$dir/matrix-mcp-darwin-x64" "$@" ;;
  *)
    echo "matrix-mcp: unsupported macOS architecture: $(uname -m)" >&2
    exit 1
    ;;
esac
