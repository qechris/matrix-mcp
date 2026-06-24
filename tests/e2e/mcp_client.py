"""Minimal MCP stdio client for driving the matrix-mcp server in E2E tests.

Speaks newline-delimited JSON-RPC over the server's stdin/stdout, performs the
initialize handshake, and exposes helpers (list_tools, call_tool) used by the
test suite.
"""
import json
import subprocess
import sys
import threading
import queue


class MCPError(RuntimeError):
    """Raised when a tool call returns a JSON-RPC error or an isError result."""


class MCPServer:
    def __init__(self, binary, env, name="mcp"):
        self.name = name
        self.init_result = None
        self.proc = subprocess.Popen(
            [binary],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=env,
            text=True,
            bufsize=1,
        )
        self._id = 0
        self._q = queue.Queue()
        threading.Thread(target=self._reader, daemon=True).start()
        threading.Thread(target=self._stderr, daemon=True).start()

    def _reader(self):
        for line in self.proc.stdout:
            line = line.strip()
            if not line:
                continue
            try:
                self._q.put(json.loads(line))
            except json.JSONDecodeError:
                pass

    def _stderr(self):
        for line in self.proc.stderr:
            sys.stderr.write(f"[{self.name}] {line}")

    def _send(self, obj):
        self.proc.stdin.write(json.dumps(obj) + "\n")
        self.proc.stdin.flush()

    def request(self, method, params=None, timeout=60):
        self._id += 1
        rid = self._id
        msg = {"jsonrpc": "2.0", "id": rid, "method": method}
        if params is not None:
            msg["params"] = params
        self._send(msg)
        while True:
            resp = self._q.get(timeout=timeout)
            if resp.get("id") == rid:
                return resp

    def initialize(self):
        resp = self.request(
            "initialize",
            {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "e2e-test", "version": "0.0.0"},
            },
        )
        self.init_result = resp["result"]
        self._send({"jsonrpc": "2.0", "method": "notifications/initialized"})
        return self.init_result

    def list_tools(self):
        return self.request("tools/list")["result"]["tools"]

    def call_tool(self, name, arguments=None, timeout=90):
        resp = self.request(
            "tools/call",
            {"name": name, "arguments": arguments or {}},
            timeout=timeout,
        )
        if "error" in resp:
            raise MCPError(f"{name}: {resp['error'].get('message', resp['error'])}")
        result = resp["result"]
        if result.get("isError"):
            raise MCPError(f"{name}: {result}")
        texts = [c["text"] for c in result.get("content", []) if c.get("type") == "text"]
        joined = "\n".join(texts)
        try:
            return json.loads(joined)
        except json.JSONDecodeError:
            return joined

    def close(self):
        try:
            self.proc.stdin.close()
            self.proc.terminate()
            self.proc.wait(timeout=10)
        except Exception:
            self.proc.kill()
