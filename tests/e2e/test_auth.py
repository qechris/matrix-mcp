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


def test_logout_without_login_fails(mcp):
    server = mcp("alice")
    with pytest.raises(MCPError, match="not logged in"):
        server.call_tool("logout")


def test_logout_clears_whoami(mcp, register_user):
    user = register_user("alice")
    server = mcp("alice")
    server.call_tool("login", {"username": user.username, "password": user.password})

    result = server.call_tool("logout")
    assert result["logged_out"] is True

    who = server.call_tool("whoami")
    assert who["logged_in"] is False


def test_logout_deletes_session_so_restart_requires_login(mcp, register_user, tmp_path):
    user = register_user("alice")
    workdir = tmp_path / "logout-persist"

    first = mcp("alice-1", workdir=workdir)
    first.call_tool("login", {"username": user.username, "password": user.password})
    first.call_tool("logout")
    first.close()

    second = mcp("alice-2", workdir=workdir)
    who = second.call_tool("whoami")
    assert who["logged_in"] is False, "logged-out session should not be restored"


def test_login_with_token_success(mcp, register_user):
    user = register_user("alice")
    server = mcp("alice")

    result = server.call_tool(
        "login_with_token",
        {
            "user_id": user.user_id,
            "device_id": user.device_id,
            "access_token": user.token,
        },
    )
    assert result["logged_in"] is True
    assert result["user_id"] == user.user_id
    assert result["device_id"] == user.device_id

    who = server.call_tool("whoami")
    assert who["logged_in"] is True
    assert who["user_id"] == user.user_id


def test_login_with_token_invalid_token_fails(mcp, register_user):
    user = register_user("alice")
    server = mcp("alice")

    with pytest.raises(MCPError):
        server.call_tool(
            "login_with_token",
            {
                "user_id": user.user_id,
                "device_id": user.device_id,
                "access_token": "syt_definitely_not_a_real_token",
            },
        )


def test_login_sso_against_unreachable_homeserver_fails_fast(mcp):
    """login_sso can't be exercised end-to-end here (no real browser/IdP in
    CI), but this pins down that a bad homeserver fails immediately via the
    supported-versions check inside get_sso_login_url, rather than hanging
    for the full 5-minute callback timeout."""
    server = mcp("alice")
    with pytest.raises(MCPError):
        server.call_tool(
            "login_sso",
            {"homeserver": "http://127.0.0.1:1"},
        )


def test_login_with_token_persists_across_restart(mcp, register_user, tmp_path):
    user = register_user("alice")
    workdir = tmp_path / "token-persist"

    first = mcp("alice-1", workdir=workdir)
    first.call_tool(
        "login_with_token",
        {
            "user_id": user.user_id,
            "device_id": user.device_id,
            "access_token": user.token,
        },
    )
    first.close()

    second = mcp("alice-2", workdir=workdir)
    who = second.call_tool("whoami")
    assert who["logged_in"] is True, "token session was not restored from disk"
    assert who["user_id"] == user.user_id
    assert who["device_id"] == user.device_id
