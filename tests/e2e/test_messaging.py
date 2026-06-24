"""Plaintext messaging tests: send, read, markdown, limit, ordering."""
from conftest import create_room, get_event, read_until


def _room(server):
    room_id = create_room(server.user.token, "Chat Room")
    server.call_tool("sync")  # make the new room known to the client
    return room_id


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
