#!/usr/bin/env bash
#
# Self-contained E2E runner: provisions a local Synapse homeserver (in a Python
# venv), builds the release binary, and runs the pytest suite against them.
#
# Requirements on the host: python3 (with venv), cargo, and a C compiler (for
# the bundled SQLite + Synapse native deps).
#
# Reuses an already-running homeserver if one is serving at $E2E_HOMESERVER.
# Override the work directory with E2E_WORK and the port with E2E_PORT.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
WORK="${E2E_WORK:-$HERE/.work}"
VENV="$WORK/venv"
HS_PORT="${E2E_PORT:-8008}"
HS_URL="${E2E_HOMESERVER:-http://localhost:${HS_PORT}}"

mkdir -p "$WORK"

echo "== ensuring python venv (synapse + pytest) =="
if [ ! -x "$VENV/bin/pytest" ]; then
  python3 -m venv "$VENV"
  "$VENV/bin/pip" install --quiet --upgrade pip
  "$VENV/bin/pip" install --quiet matrix-synapse pytest
fi

echo "== building release binary =="
( cd "$ROOT" && cargo build --release )
export MATRIX_MCP_BIN="$ROOT/target/release/matrix-mcp"
export E2E_HOMESERVER="$HS_URL"

STARTED=""
if ! curl -sf "$HS_URL/_matrix/client/versions" >/dev/null 2>&1; then
  echo "== generating + starting synapse =="
  if [ ! -f "$WORK/homeserver.yaml" ]; then
    "$VENV/bin/python" -m synapse.app.homeserver \
      --server-name localhost -c "$WORK/homeserver.yaml" \
      --generate-config --report-stats=no --data-directory "$WORK/data" >/dev/null
    "$VENV/bin/python" - "$WORK/homeserver.yaml" <<'PY'
import sys
p = sys.argv[1]
# Drop the IPv6 loopback bind (fails in many sandboxes) and relax limits.
lines = [l for l in open(p).read().splitlines() if l.strip() != "- ::1"]
extra = """
enable_registration: true
enable_registration_without_verification: true
rc_message: {per_second: 1000, burst_count: 1000}
rc_registration: {per_second: 1000, burst_count: 1000}
rc_login:
  address: {per_second: 1000, burst_count: 1000}
  account: {per_second: 1000, burst_count: 1000}
  failed_attempts: {per_second: 1000, burst_count: 1000}
"""
open(p, "w").write("\n".join(lines) + "\n" + extra)
PY
  fi
  "$VENV/bin/python" -m synapse.app.homeserver -c "$WORK/homeserver.yaml" \
    >"$WORK/synapse.log" 2>&1 &
  echo $! > "$WORK/synapse.pid"
  STARTED=1
  for _ in $(seq 1 60); do
    curl -sf "$HS_URL/_matrix/client/versions" >/dev/null 2>&1 && break
    sleep 1
  done
fi

echo "== running pytest =="
set +e
"$VENV/bin/pytest" "$HERE" -v "$@"
RC=$?
set -e

if [ -n "$STARTED" ] && [ -f "$WORK/synapse.pid" ]; then
  echo "== stopping synapse =="
  kill "$(cat "$WORK/synapse.pid")" 2>/dev/null || true
fi

exit $RC
