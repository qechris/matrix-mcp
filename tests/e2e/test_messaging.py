"""Plaintext messaging tests: send, read, markdown, limit, ordering, edit,
redact, react, reply, and read receipts."""
import pytest

from conftest import create_room_and_sync, get_event, hs_http, read_until
from mcp_client import MCPError


def _room(server):
    return create_room_and_sync(server, "Chat Room")


def test_send_and_read_plaintext(logged_in):
    alice = logged_in("alice")
    room = _room(alice)

    sent = alice.call_tool("send_message", {"room_id": room, "body": "hello world"})
    assert sent["sent"] is True
    assert sent["event_id"].startswith("$")

    msg = read_until(alice, room, lambda m: m.get("body") == "hello world")
    assert isinstance(msg, dict), f"message not found: {msg}"
    assert msg["sender"] == alice.user.user_id
    assert msg["unable_to_decrypt"] is False
    assert msg["msgtype"] == "m.text"


def test_markdown_formatting(logged_in):
    alice = logged_in("alice")
    room = _room(alice)

    sent = alice.call_tool(
        "send_message",
        {"room_id": room, "body": "**bold** and _italic_", "markdown": True},
    )
    event = get_event(alice.user.token, room, sent["event_id"])
    content = event["content"]
    assert content["format"] == "org.matrix.custom.html"
    assert "<strong>bold</strong>" in content["formatted_body"]
    assert "<em>italic</em>" in content["formatted_body"]


def test_read_respects_limit(logged_in):
    alice = logged_in("alice")
    room = _room(alice)
    for i in range(5):
        alice.call_tool("send_message", {"room_id": room, "body": f"msg {i}"})

    result = alice.call_tool("read_messages", {"room_id": room, "limit": 3})
    assert result["count"] == 3
    assert len(result["messages"]) == 3


def test_read_chronological_order(logged_in):
    alice = logged_in("alice")
    room = _room(alice)
    bodies = ["first", "second", "third"]
    for b in bodies:
        alice.call_tool("send_message", {"room_id": room, "body": b})

    result = alice.call_tool("read_messages", {"room_id": room, "limit": 20})
    seen = [m["body"] for m in result["messages"] if m.get("msgtype") == "m.text"]
    # The three sent messages must appear in order at the end of the timeline.
    assert seen[-3:] == bodies, seen


def test_read_messages_pagination(logged_in):
    alice = logged_in("alice")
    room = _room(alice)
    bodies = [f"msg{i}" for i in range(5)]
    for b in bodies:
        alice.call_tool("send_message", {"room_id": room, "body": b})

    page1 = alice.call_tool("read_messages", {"room_id": room, "limit": 2})
    assert page1["count"] == 2
    assert page1["next_token"] is not None
    assert [m["body"] for m in page1["messages"]] == bodies[-2:]

    page2 = alice.call_tool(
        "read_messages",
        {"room_id": room, "limit": 2, "before_token": page1["next_token"]},
    )
    assert page2["count"] == 2
    assert [m["body"] for m in page2["messages"]] == bodies[-4:-2]

    page1_ids = {m["event_id"] for m in page1["messages"]}
    page2_ids = {m["event_id"] for m in page2["messages"]}
    assert page1_ids.isdisjoint(page2_ids)


def test_edit_message(logged_in):
    alice = logged_in("alice")
    room = _room(alice)
    sent = alice.call_tool("send_message", {"room_id": room, "body": "typo"})

    edited = alice.call_tool(
        "edit_message", {"room_id": room, "event_id": sent["event_id"], "body": "fixed"}
    )
    assert edited["edited"] is True

    event = get_event(alice.user.token, room, edited["event_id"])
    content = event["content"]
    assert content["m.new_content"]["body"] == "fixed"
    assert content["m.relates_to"]["rel_type"] == "m.replace"
    assert content["m.relates_to"]["event_id"] == sent["event_id"]


def test_edit_of_someone_elses_message_fails(logged_in):
    alice = logged_in("alice")
    bob = logged_in("bob")
    room = _room(alice)
    hs_http(
        "POST",
        f"/_matrix/client/v3/rooms/{room}/invite",
        token=alice.user.token,
        body={"user_id": bob.user.user_id},
    )
    bob.call_tool("join_room", {"room": room})

    sent = alice.call_tool("send_message", {"room_id": room, "body": "alice's message"})
    bob.call_tool("sync")

    with pytest.raises(MCPError):
        bob.call_tool(
            "edit_message", {"room_id": room, "event_id": sent["event_id"], "body": "hijacked"}
        )


def test_redact_message(logged_in):
    alice = logged_in("alice")
    room = _room(alice)
    sent = alice.call_tool("send_message", {"room_id": room, "body": "oops"})

    redacted = alice.call_tool("redact_event", {"room_id": room, "event_id": sent["event_id"]})
    assert redacted["redacted"] is True

    event = get_event(alice.user.token, room, sent["event_id"])
    assert event["content"] == {}


def test_send_and_redact_reaction(logged_in):
    alice = logged_in("alice")
    room = _room(alice)
    sent = alice.call_tool("send_message", {"room_id": room, "body": "react to me"})

    reacted = alice.call_tool(
        "send_reaction", {"room_id": room, "event_id": sent["event_id"], "emoji": "\U0001F44D"}
    )
    assert reacted["reacted"] is True

    reaction_event = get_event(alice.user.token, room, reacted["event_id"])
    relates_to = reaction_event["content"]["m.relates_to"]
    assert relates_to["rel_type"] == "m.annotation"
    assert relates_to["event_id"] == sent["event_id"]
    assert relates_to["key"] == "\U0001F44D"

    # Un-react by redacting the reaction event itself.
    alice.call_tool("redact_event", {"room_id": room, "event_id": reacted["event_id"]})
    reaction_event = get_event(alice.user.token, room, reacted["event_id"])
    assert reaction_event["content"] == {}


def test_reply_to_message(logged_in):
    alice = logged_in("alice")
    room = _room(alice)
    original = alice.call_tool("send_message", {"room_id": room, "body": "original"})

    reply = alice.call_tool(
        "send_message",
        {"room_id": room, "body": "a reply", "reply_to_event_id": original["event_id"]},
    )
    event = get_event(alice.user.token, room, reply["event_id"])
    in_reply_to = event["content"]["m.relates_to"]["m.in_reply_to"]
    assert in_reply_to["event_id"] == original["event_id"]


def test_mark_read_defaults_to_latest_message(logged_in):
    alice = logged_in("alice")
    room = _room(alice)
    alice.call_tool("send_message", {"room_id": room, "body": "first"})
    last = alice.call_tool("send_message", {"room_id": room, "body": "last"})

    marked = alice.call_tool("mark_read", {"room_id": room})
    assert marked["marked_read"] is True
    assert marked["event_id"] == last["event_id"]


def test_mark_read_explicit_event(logged_in):
    alice = logged_in("alice")
    room = _room(alice)
    first = alice.call_tool("send_message", {"room_id": room, "body": "first"})
    alice.call_tool("send_message", {"room_id": room, "body": "last"})

    marked = alice.call_tool(
        "mark_read", {"room_id": room, "event_id": first["event_id"]}
    )
    assert marked["event_id"] == first["event_id"]
