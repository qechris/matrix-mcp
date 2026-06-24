"""SSE / streamable-HTTP transport test: drive the server over HTTP."""
import json
import os
import socket
import subprocess
import time
import urllib.request

import pytest

from conftest import BIN


def _free_port():
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


@pytest.fixture
def sse_server(tmp_path):
    port = _free_port()
    env = dict(os.environ)
    env.update(
        {
            "MATRIX_MCP_TRANSPORT": "sse",
            "MATRIX_MCP_ADDRESS": f"127.0.0.1:{port}",
            "MATRIX_MCP_PATH": "/mcp",
            "MATRIX_STORE_PATH": str(tmp_path / "store"),
            "MATRIX_SESSION_FILE": str(tmp_path / "session.json"),
            "RUST_LOG": "matrix_mcp=warn",
        }
    )
    proc = subprocess.Popen([BIN], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    url = f"http://127.0.0.1:{port}/mcp"
    # Wait for the listener to accept connections.
    for _ in range(50):
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=0.5):
                break
        except OSError:
            time.sleep(0.2)
    yield url
    proc.terminate()
    try:
        proc.wait(timeout=10)
    except Exception:
        proc.kill()


def _post_initialize(url):
    body = json.dumps(
        {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "sse-test", "version": "0.0.0"},
            },
        }
    ).encode()
    req = urllib.request.Request(url, data=body, method="POST")
    req.add_header("Content-Type", "application/json")
    req.add_header("Accept", "application/json, text/event-stream")
    with urllib.request.urlopen(req, timeout=15) as r:
        return r, r.read().decode()


def test_sse_initialize(sse_server):
    resp, raw = _post_initialize(sse_server)
    assert resp.status == 200
    assert "text/event-stream" in resp.headers.get("content-type", "")
    assert resp.headers.get("mcp-session-id")

    # Parse the SSE payload: find the data: line carrying the JSON-RPC result.
    payloads = [
        line[len("data:"):].strip()
        for line in raw.splitlines()
        if line.startswith("data:") and line[len("data:"):].strip().startswith("{")
    ]
    assert payloads, f"no JSON-RPC data frame in SSE response: {raw!r}"
    result = json.loads(payloads[0])["result"]
    assert result["serverInfo"]["name"] == "matrix-mcp"
    assert "tools" in result["capabilities"]
