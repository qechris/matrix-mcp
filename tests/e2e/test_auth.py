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


# --- Stale state left behind by a previous install --------------------------
#
# The session file and SQLite crypto store live under ~/.local/state/matrix-mcp,
# outside the Claude Desktop extension directory, so uninstalling the extension
# leaves them behind. A reinstall then starts against a previous device's
# state. These tests pin down that a fresh login always recovers from that.


def test_relogin_after_logout_on_same_store(mcp, register_user, tmp_path):
    """Logging out and back in mints a new device; the store still holds the
    old device's crypto identity, which must not block the new login."""
    user = register_user("alice")
    workdir = tmp_path / "relogin"

    first = mcp("alice-1", workdir=workdir)
    old = first.call_tool("login", {"username": user.username, "password": user.password})
    first.call_tool("logout")
    first.close()

    second = mcp("alice-2", workdir=workdir)
    new = second.call_tool("login", {"username": user.username, "password": user.password})
    assert new["logged_in"] is True
    assert new["device_id"] != old["device_id"]
    who = second.call_tool("whoami")
    assert who["logged_in"] is True
    assert who["device_id"] == new["device_id"]


def test_login_as_different_user_on_same_store(mcp, register_user, tmp_path):
    """A store left behind by one account must not break logging in as another."""
    alice = register_user("alice")
    bob = register_user("bob")
    workdir = tmp_path / "switch-user"

    first = mcp("alice", workdir=workdir)
    first.call_tool("login", {"username": alice.username, "password": alice.password})
    first.call_tool("logout")
    first.close()

    second = mcp("bob", workdir=workdir)
    result = second.call_tool("login", {"username": bob.username, "password": bob.password})
    assert result["logged_in"] is True
    assert result["user_id"] == bob.user_id


def test_reinstall_with_revoked_device_recovers(mcp, register_user, tmp_path, homeserver):
    """Simulates uninstalling, removing the device from another client, then
    reinstalling: the leftover session file points at a device whose token no
    longer works. The server must start cleanly, report not logged in, and
    accept a fresh login on the same state directory."""
    from conftest import hs_http

    user = register_user("alice")
    workdir = tmp_path / "reinstall"

    first = mcp("alice-1", workdir=workdir)
    first.call_tool("login", {"username": user.username, "password": user.password})
    first.close()

    # Revoke the device's token out from under the leftover session file.
    import json
    session = json.loads((workdir / "session.json").read_text())
    token = session["session"]["access_token"]
    status, _ = hs_http("POST", "/_matrix/client/v3/logout", token=token, body={})
    assert status == 200

    second = mcp("alice-2", workdir=workdir)
    who = second.call_tool("whoami")
    assert who["logged_in"] is False, "a revoked session must not be reported as logged in"

    result = second.call_tool("login", {"username": user.username, "password": user.password})
    assert result["logged_in"] is True


def test_logout_removes_local_state(mcp, register_user, tmp_path):
    """Logout deletes the device server-side, so nothing it stored locally is
    reusable: the session file and the store's SQLite files are removed."""
    user = register_user("alice")
    workdir = tmp_path / "cleanup"

    server = mcp("alice", workdir=workdir)
    server.call_tool("login", {"username": user.username, "password": user.password})
    assert (workdir / "session.json").exists()
    assert any((workdir / "store").glob("matrix-sdk-*.sqlite3*"))

    server.call_tool("logout")
    assert not (workdir / "session.json").exists()
    assert not list((workdir / "store").glob("matrix-sdk-*.sqlite3*"))


def test_login_while_logged_in_is_refused_and_keeps_session(mcp, register_user):
    """A second login would need the store to itself; it is refused with a
    clear message instead of discarding (or corrupting) the working session."""
    alice = register_user("alice")
    bob = register_user("bob")
    server = mcp("alice")
    first = server.call_tool("login", {"username": alice.username, "password": alice.password})

    with pytest.raises(MCPError, match="already logged in"):
        server.call_tool("login", {"username": bob.username, "password": bob.password})

    who = server.call_tool("whoami")
    assert who["logged_in"] is True
    assert who["user_id"] == alice.user_id
    assert who["device_id"] == first["device_id"]


def test_logout_after_token_revoked_elsewhere_recovers(mcp, register_user, tmp_path):
    """If the device is removed from another client while the server runs, its
    token dies. Logout must still clear the session, or it could neither log
    out nor be replaced, since logging in over a live session is refused."""
    import json
    from conftest import hs_http

    user = register_user("alice")
    workdir = tmp_path / "revoked-live"
    server = mcp("alice", workdir=workdir)
    server.call_tool("login", {"username": user.username, "password": user.password})

    token = json.loads((workdir / "session.json").read_text())["session"]["access_token"]
    status, _ = hs_http("POST", "/_matrix/client/v3/logout", token=token, body={})
    assert status == 200

    result = server.call_tool("logout")
    assert result["logged_out"] is True
    assert server.call_tool("whoami")["logged_in"] is False

    again = server.call_tool("login", {"username": user.username, "password": user.password})
    assert again["logged_in"] is True


def _leave_orphaned_store(mcp, user, workdir):
    """Log in, then remove only the session file - the state v0.2.3 and earlier
    left behind after `logout`, and what a reinstall on such a machine finds."""
    server = mcp("orphan", workdir=workdir)
    login = server.call_tool("login", {"username": user.username, "password": user.password})
    server.close()
    (workdir / "session.json").unlink()
    assert any((workdir / "store").glob("matrix-sdk-crypto.sqlite3*"))
    return login["device_id"]


def test_password_login_over_orphaned_store(mcp, register_user, tmp_path):
    user = register_user("alice")
    workdir = tmp_path / "orphan-password"
    old_device = _leave_orphaned_store(mcp, user, workdir)

    server = mcp("alice-2", workdir=workdir)
    result = server.call_tool("login", {"username": user.username, "password": user.password})
    assert result["logged_in"] is True
    assert result["device_id"] != old_device


def test_token_login_over_orphaned_store_of_another_device(mcp, register_user, tmp_path):
    """Token login knows its device up front and keeps a matching store; a
    store from a different device is discarded and the login retried."""
    user = register_user("alice")
    workdir = tmp_path / "orphan-token"
    _leave_orphaned_store(mcp, user, workdir)

    server = mcp("alice-2", workdir=workdir)
    # user.token/device_id come from registration: a different device from the
    # one that owns the orphaned store.
    result = server.call_tool(
        "login_with_token",
        {"user_id": user.user_id, "device_id": user.device_id, "access_token": user.token},
    )
    assert result["logged_in"] is True
    assert result["device_id"] == user.device_id


def test_restore_with_session_and_store_from_different_devices(mcp, register_user, tmp_path):
    """A session file for one device next to a store owned by another (e.g. a
    partially cleaned-up install) must be discarded at startup, not left to
    break every later login."""
    user = register_user("alice")
    workdir = tmp_path / "mismatch"

    first = mcp("alice-1", workdir=workdir)
    first.call_tool("login", {"username": user.username, "password": user.password})
    first.close()
    stale_session = (workdir / "session.json").read_text()

    second = mcp("alice-2", workdir=workdir)
    second.call_tool("logout")
    second.call_tool("login", {"username": user.username, "password": user.password})
    second.close()
    # Pair the second device's store with the first device's session file.
    (workdir / "session.json").write_text(stale_session)

    third = mcp("alice-3", workdir=workdir)
    assert third.call_tool("whoami")["logged_in"] is False
    assert not (workdir / "session.json").exists()
    result = third.call_tool("login", {"username": user.username, "password": user.password})
    assert result["logged_in"] is True
