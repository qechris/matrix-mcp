"""Thread reading: root + replies via `read_thread`, and the `thread_root`
marker `read_messages` puts on threaded replies.

The server has no thread-sending tool, so threaded replies are posted straight
to the homeserver's client-server API.
"""
import time

import pytest

from conftest import create_room_and_sync, get_event, hs_http, read_until
from mcp_client import MCPError


def _room(server):
    return create_room_and_sync(server, "Thread Room")


def _send_threaded(token, room_id, root_event_id, body, reply_to=None):
    """Post a threaded reply (`m.thread`) directly via the client-server API."""
    txn = f"thread-{time.time_ns()}"
    status, resp = hs_http(
        "PUT",
        f"/_matrix/client/v3/rooms/{room_id}/send/m.room.message/{txn}",
        token=token,
        body={
            "msgtype": "m.text",
            "body": body,
            "m.relates_to": {
                "rel_type": "m.thread",
                "event_id": root_event_id,
                # Clients that don't understand threads render the reply as a
                # rich reply to the last thread message; Element sends this and
                # so do we, to keep the events realistic.
                "is_falling_back": True,
                "m.in_reply_to": {"event_id": reply_to or root_event_id},
            },
        },
    )
    assert status == 200, f"threaded send failed: {resp}"
    return resp["event_id"]


def _thread_of(server, room_id, bodies):
    """Create a thread rooted at "root" with `bodies` as replies."""
    root = server.call_tool("send_message", {"room_id": room_id, "body": "root"})["event_id"]
    previous = root
    replies = []
    for body in bodies:
        previous = _send_threaded(server.user.token, room_id, root, body, reply_to=previous)
        replies.append(previous)
    server.call_tool("sync")
    return root, replies


def test_read_thread_returns_root_then_replies(logged_in):
    alice = logged_in("alice")
    room = _room(alice)
    root, replies = _thread_of(alice, room, ["first reply", "second reply"])

    thread = alice.call_tool("read_thread", {"room_id": room, "event_id": root})

    assert thread["thread_root"] == root
    assert thread["reply_count"] == 2
    assert thread["count"] == 3
    assert [m["body"] for m in thread["messages"]] == ["root", "first reply", "second reply"]
    assert [m["event_id"] for m in thread["messages"]] == [root, *replies]
    # The root isn't itself in the thread; every reply names the root it's in.
    assert thread["messages"][0]["thread_root"] is None
    assert all(m["thread_root"] == root for m in thread["messages"][1:])
    assert thread["undecryptable_count"] == 0


def test_read_thread_accepts_a_reply_event_id(logged_in):
    """An id taken from `read_messages` is a reply, not the root - it should
    still resolve to the whole thread rather than an empty result."""
    alice = logged_in("alice")
    room = _room(alice)
    root, replies = _thread_of(alice, room, ["first reply", "second reply"])

    thread = alice.call_tool("read_thread", {"room_id": room, "event_id": replies[1]})

    assert thread["thread_root"] == root
    assert [m["body"] for m in thread["messages"]] == ["root", "first reply", "second reply"]


def test_read_thread_respects_limit_and_pages(logged_in):
    alice = logged_in("alice")
    room = _room(alice)
    root, _ = _thread_of(alice, room, [f"reply {i}" for i in range(5)])

    first = alice.call_tool("read_thread", {"room_id": room, "event_id": root, "limit": 2})
    # Root plus the first two replies.
    assert [m["body"] for m in first["messages"]] == ["root", "reply 0", "reply 1"]
    assert first["reply_count"] == 2
    assert first["next_token"]

    second = alice.call_tool(
        "read_thread",
        {"room_id": room, "event_id": root, "limit": 2, "from_token": first["next_token"]},
    )
    # Later pages continue with replies only - the root isn't repeated.
    assert [m["body"] for m in second["messages"]] == ["reply 2", "reply 3"]


def test_read_thread_on_message_without_replies(logged_in):
    alice = logged_in("alice")
    room = _room(alice)
    lonely = alice.call_tool("send_message", {"room_id": room, "body": "no replies here"})

    thread = alice.call_tool("read_thread", {"room_id": room, "event_id": lonely["event_id"]})

    assert thread["reply_count"] == 0
    assert [m["body"] for m in thread["messages"]] == ["no replies here"]
    assert "no threaded replies" in thread["hint"]


def test_read_thread_rejects_unknown_event(logged_in):
    alice = logged_in("alice")
    room = _room(alice)

    with pytest.raises(MCPError):
        alice.call_tool("read_thread", {"room_id": room, "event_id": "$nonexistent"})


def test_read_messages_marks_threaded_replies(logged_in):
    """`read_messages` returns the room timeline, where thread replies are
    interleaved with everything else - `thread_root` is what tells them apart
    and points at the thread to read."""
    alice = logged_in("alice")
    room = _room(alice)
    root, _ = _thread_of(alice, room, ["in the thread"])
    alice.call_tool("send_message", {"room_id": room, "body": "in the room"})

    reply = read_until(alice, room, lambda m: m.get("body") == "in the thread")
    assert isinstance(reply, dict), f"threaded reply not found: {reply}"
    assert reply["thread_root"] == root

    messages = alice.call_tool("read_messages", {"room_id": room, "limit": 30})["messages"]
    by_body = {m["body"]: m for m in messages if m.get("body")}
    assert by_body["root"]["thread_root"] is None
    assert by_body["in the room"]["thread_root"] is None


def test_read_thread_in_encrypted_room(logged_in):
    alice = logged_in("alice")
    room = create_room_and_sync(alice, "Encrypted Threads", encrypted=True)
    root = alice.call_tool("send_message", {"room_id": room, "body": "encrypted root"})["event_id"]
    alice.call_tool("sync")

    thread = alice.call_tool("read_thread", {"room_id": room, "event_id": root})

    assert thread["thread_root"] == root
    assert thread["messages"][0]["body"] == "encrypted root"
    assert thread["messages"][0]["unable_to_decrypt"] is False
    assert thread["undecryptable_count"] == 0


def test_reply_to_threaded_message_stays_in_thread(logged_in):
    alice = logged_in("alice")
    room = _room(alice)
    root, replies = _thread_of(alice, room, ["in the thread"])

    sent = alice.call_tool(
        "send_message",
        {"room_id": room, "body": "me too", "reply_to_event_id": replies[0]},
    )
    assert sent["thread_root"] == root

    alice.call_tool("sync")
    thread = alice.call_tool("read_thread", {"room_id": room, "event_id": root})
    assert [m["body"] for m in thread["messages"]] == ["root", "in the thread", "me too"]


def test_reply_to_unthreaded_message_stays_in_timeline(logged_in):
    alice = logged_in("alice")
    room = _room(alice)
    original = alice.call_tool("send_message", {"room_id": room, "body": "original"})

    sent = alice.call_tool(
        "send_message",
        {"room_id": room, "body": "a reply", "reply_to_event_id": original["event_id"]},
    )

    assert sent["thread_root"] is None
    event = get_event(alice.user.token, room, sent["event_id"])
    assert "rel_type" not in event["content"]["m.relates_to"]


def test_reply_to_thread_root_is_not_threaded(logged_in):
    """Threads are forwarded, not started: the root carries no thread relation
    of its own, so replying to it is an ordinary rich reply - the same as
    replying from Element's main timeline."""
    alice = logged_in("alice")
    room = _room(alice)
    root, _ = _thread_of(alice, room, ["in the thread"])

    sent = alice.call_tool(
        "send_message", {"room_id": room, "body": "at the root", "reply_to_event_id": root}
    )

    assert sent["thread_root"] is None
