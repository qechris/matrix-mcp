"""Server-side key backup: enabling it, and using it to decrypt history on a
device that never received the original room keys."""
import pytest

from conftest import create_room_and_sync, read_until
from mcp_client import MCPError


def test_enable_key_backup_returns_recovery_key(logged_in):
    alice = logged_in("alice")

    result = alice.call_tool("enable_key_backup")
    assert result["enabled"] is True
    assert result["recovery_key"], "no recovery key returned"
    assert result["backup_state"] == "Enabled"


def test_enable_key_backup_twice_fails(logged_in):
    alice = logged_in("alice")
    alice.call_tool("enable_key_backup")

    # A backup already exists on the account, so a second attempt must not
    # silently replace it (that would strand the keys under the first key).
    with pytest.raises(MCPError):
        alice.call_tool("enable_key_backup")


def test_restore_key_backup_without_backup_fails(logged_in):
    alice = logged_in("alice")
    with pytest.raises(MCPError):
        alice.call_tool("restore_key_backup", {"recovery_key": "EsTb EsTb EsTb EsTb"})


def test_download_room_keys_without_backup_reports_unusable(logged_in):
    alice = logged_in("alice")
    room = create_room_and_sync(alice, "No Backup")

    result = alice.call_tool("download_room_keys", {"room_id": room})
    assert result["key_backup_usable"] is False
    assert "restore_key_backup" in result["hint"]


def test_new_device_reads_history_after_restoring_backup(mcp, register_user, tmp_path):
    """The whole point: a second device, which never held the room key, can
    read messages sent before it existed once the backup is unlocked."""
    user = register_user("alice")

    # First device: enable backup, send a message into an encrypted room.
    first = mcp("alice-1", workdir=tmp_path / "device-1")
    first.call_tool("login", {"username": user.username, "password": user.password})
    first.user = user
    recovery_key = first.call_tool("enable_key_backup")["recovery_key"]

    room = create_room_and_sync(first, "History", encrypted=True)
    first.call_tool("send_message", {"room_id": room, "body": "sent before device 2 existed"})
    # Push the room key into the backup before the device goes away.
    first.call_tool("sync")
    first.close()

    # Second device: separate store, so it has none of the first device's keys.
    second = mcp("alice-2", workdir=tmp_path / "device-2")
    second.call_tool("login", {"username": user.username, "password": user.password})
    second.user = user
    second.call_tool("sync")

    before = second.call_tool("read_messages", {"room_id": room, "limit": 20})
    assert before["undecryptable_count"] > 0, (
        f"expected history to be undecryptable before restoring the backup: {before}"
    )

    restored = second.call_tool("restore_key_backup", {"recovery_key": recovery_key})
    assert restored["key_backup_usable"] is True, restored

    msg = read_until(
        second, room, lambda m: m.get("body") == "sent before device 2 existed"
    )
    assert isinstance(msg, dict), f"history still unreadable after restore: {msg}"
    assert msg["unable_to_decrypt"] is False
