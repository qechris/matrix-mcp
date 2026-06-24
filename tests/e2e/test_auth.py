"""Authentication and session-persistence tests."""
import pytest

from mcp_client import MCPError


def test_login_success(mcp, register_user):
    user = register_user("alice")
    server = mcp("alice")
    result = server.call_tool("login", {"username": user.username, "password": user.password})
    assert result["logged_in"] is True
    assert result["user_id"] == user.user_id
    assert result["device_id"]


def test_login_wrong_password(mcp, register_user):
    user = register_user("alice")
    server = mcp("alice")
    with pytest.raises(MCPError):
        server.call_tool("login", {"username": user.username, "password": "definitely-wrong"})


def test_whoami_after_login(mcp, register_user, homeserver):
    user = register_user("alice")
    server = mcp("alice")
    server.call_tool("login", {"username": user.username, "password": user.password})
    who = server.call_tool("whoami")
    assert who["logged_in"] is True
    assert who["user_id"] == user.user_id
    assert who["device_id"]
    assert who["homeserver"].rstrip("/") == homeserver.rstrip("/")


def test_session_persists_across_restart(mcp, register_user, tmp_path):
    """A second process pointed at the same store/session restores the login
    without calling `login` again."""
    user = register_user("alice")
    workdir = tmp_path / "persist"

    first = mcp("alice-1", workdir=workdir)
    login = first.call_tool("login", {"username": user.username, "password": user.password})
    device_id = login["device_id"]
    first.close()

    second = mcp("alice-2", workdir=workdir)
    who = second.call_tool("whoami")
    assert who["logged_in"] is True, "session was not restored from disk"
    assert who["user_id"] == user.user_id
    assert who["device_id"] == device_id, "restored a different device"
