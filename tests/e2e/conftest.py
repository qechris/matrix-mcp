"""Shared pytest fixtures and helpers for the matrix-mcp E2E suite.

These tests drive the real `matrix-mcp` release binary over MCP stdio against a
live Matrix homeserver. `run.sh` provisions the homeserver and binary and sets
the environment variables below; the suite can also run standalone if those are
already in place.

Environment:
  E2E_HOMESERVER   base URL of the homeserver (default http://localhost:8008)
  MATRIX_MCP_BIN   path to the matrix-mcp binary (default target/release/matrix-mcp)
"""
import json
import os
import pathlib
import time
import uuid
import urllib.error
import urllib.request

import pytest

from mcp_client import MCPServer

HOMESERVER = os.environ.get("E2E_HOMESERVER", "http://localhost:8008")
DEFAULT_BIN = pathlib.Path(__file__).resolve().parents[2] / "target" / "release" / "matrix-mcp"
BIN = os.environ.get("MATRIX_MCP_BIN", str(DEFAULT_BIN))


def hs_http(method, path, token=None, body=None, timeout=30):
    """Make a raw client-server API call against the homeserver."""
    url = HOMESERVER + path
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", "Bearer " + token)
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return r.status, json.load(r)
    except urllib.error.HTTPError as e:
        try:
            return e.code, json.load(e)
        except Exception:
            return e.code, {}


def _register(username, password):
    status, resp = hs_http(
        "POST",
        "/_matrix/client/v3/register",
        body={"username": username, "password": password, "auth": {"type": "m.login.dummy"}},
    )
    if status == 200:
        return resp["access_token"], resp["user_id"]
    # Fall back to login if the account already exists.
    status, resp = hs_http(
        "POST",
        "/_matrix/client/v3/login",
        body={
            "type": "m.login.password",
            "identifier": {"type": "m.id.user", "user": username},
            "password": password,
        },
    )
    assert status == 200, f"register/login failed for {username}: {resp}"
    return resp["access_token"], resp["user_id"]


@pytest.fixture(scope="session")
def homeserver():
    """Ensure the homeserver is reachable; skip the suite otherwise."""
    for _ in range(60):
        try:
            with urllib.request.urlopen(HOMESERVER + "/_matrix/client/versions", timeout=5) as r:
                if r.status == 200:
                    return HOMESERVER
        except Exception:
            time.sleep(1)
    pytest.skip(f"homeserver not reachable at {HOMESERVER}")


@pytest.fixture(scope="session", autouse=True)
def _binary_exists():
    if not pathlib.Path(BIN).exists():
        pytest.skip(f"matrix-mcp binary not found at {BIN} (build it with `cargo build --release`)")


class User(dict):
    @property
    def username(self):
        return self["username"]

    @property
    def password(self):
        return self["password"]

    @property
    def user_id(self):
        return self["user_id"]

    @property
    def token(self):
        return self["token"]


@pytest.fixture
def register_user(homeserver):
    """Factory that registers a fresh, uniquely-named user on the homeserver."""
    def _make(prefix="user"):
        username = f"{prefix}-{uuid.uuid4().hex[:10]}"
        password = "pw-" + uuid.uuid4().hex[:12]
        token, user_id = _register(username, password)
        return User(username=username, password=password, user_id=user_id, token=token)

    return _make


@pytest.fixture
def mcp(tmp_path, homeserver):
    """Factory for MCP server instances with isolated store/session directories.

    Returns a callable: mcp(name="alice", workdir=None) -> initialized MCPServer.
    Passing a shared workdir lets a test restart a server against the same store
    (to exercise session persistence).
    """
    servers = []

    def _make(name="mcp", workdir=None):
        d = pathlib.Path(workdir) if workdir else (tmp_path / name)
        d.mkdir(parents=True, exist_ok=True)
        env = dict(os.environ)
        env.update(
            {
                "MATRIX_HOMESERVER": homeserver,
                "MATRIX_STORE_PATH": str(d / "store"),
                "MATRIX_SESSION_FILE": str(d / "session.json"),
                "RUST_LOG": "matrix_mcp=warn,matrix_sdk=error",
            }
        )
        server = MCPServer(BIN, env, name=name)
        server.initialize()
        servers.append(server)
        return server

    yield _make

    for s in servers:
        s.close()


@pytest.fixture
def logged_in(mcp, register_user):
    """Factory returning an MCP server already logged in as a fresh user.

    The originating User is attached as `server.user` for room/token access.
    """
    def _make(name="user"):
        user = register_user(name)
        server = mcp(name)
        server.call_tool("login", {"username": user.username, "password": user.password})
        server.user = user
        return server

    return _make


def create_room(token, name, invite=None, encrypted=False, alias=None, topic=None, public=False):
    """Create a room via the client-server API and return its room id."""
    body = {"name": name, "invite": invite or []}
    if public:
        # public_chat preset sets join_rules=public so anyone can join by alias.
        body["preset"] = "public_chat"
        body["visibility"] = "public"
    if topic:
        body["topic"] = topic
    if alias:
        body["room_alias_name"] = alias
    if encrypted:
        body["initial_state"] = [
            {
                "type": "m.room.encryption",
                "state_key": "",
                "content": {"algorithm": "m.megolm.v1.aes-sha2"},
            }
        ]
    status, resp = hs_http("POST", "/_matrix/client/v3/createRoom", token=token, body=body)
    assert status == 200, f"createRoom failed: {resp}"
    return resp["room_id"]


def get_event(token, room_id, event_id):
    """Fetch a single event via the client-server API."""
    status, resp = hs_http(
        "GET", f"/_matrix/client/v3/rooms/{room_id}/event/{event_id}", token=token
    )
    assert status == 200, f"get event failed: {resp}"
    return resp


def read_until(server, room_id, predicate, tries=8, sleep=2.0):
    """Sync + read repeatedly until `predicate(message)` is true for some message."""
    last = None
    for _ in range(tries):
        try:
            server.call_tool("sync")
        except Exception:
            pass
        msgs = server.call_tool("read_messages", {"room_id": room_id, "limit": 30})
        last = msgs
        for m in msgs.get("messages", []):
            if predicate(m):
                return m
        time.sleep(sleep)
    return None if last is None else last.get("messages")
