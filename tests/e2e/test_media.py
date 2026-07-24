"""Media attachment send/download tests."""
import pytest

from conftest import create_room_and_sync, get_event
from mcp_client import MCPError


def _room(server):
    return create_room_and_sync(server, "Media Room")


def test_send_and_download_file(logged_in, tmp_path):
    alice = logged_in("alice")
    room = _room(alice)

    src = tmp_path / "hello.txt"
    src.write_text("hello from matrix-mcp")

    sent = alice.call_tool("send_file", {"room_id": room, "path": str(src)})
    assert sent["sent"] is True

    event = get_event(alice.user.token, room, sent["event_id"])
    assert event["content"]["msgtype"] == "m.file"
    assert event["content"]["body"] == "hello.txt"

    dest = tmp_path / "downloaded.txt"
    downloaded = alice.call_tool(
        "download_media",
        {"room_id": room, "event_id": sent["event_id"], "save_path": str(dest)},
    )
    assert downloaded["filename"] == "hello.txt"
    assert downloaded["bytes_written"] == len("hello from matrix-mcp")
    assert dest.read_text() == "hello from matrix-mcp"


def test_send_file_with_caption(logged_in, tmp_path):
    alice = logged_in("alice")
    room = _room(alice)

    src = tmp_path / "notes.txt"
    src.write_text("file body")

    sent = alice.call_tool(
        "send_file", {"room_id": room, "path": str(src), "caption": "check this out"}
    )
    event = get_event(alice.user.token, room, sent["event_id"])
    assert event["content"]["body"] == "check this out"
    assert event["content"]["filename"] == "notes.txt"


def test_download_media_of_non_media_event_fails(logged_in, tmp_path):
    alice = logged_in("alice")
    room = _room(alice)
    sent = alice.call_tool("send_message", {"room_id": room, "body": "just text"})

    with pytest.raises(MCPError):
        alice.call_tool(
            "download_media",
            {
                "room_id": room,
                "event_id": sent["event_id"],
                "save_path": str(tmp_path / "whatever"),
            },
        )
