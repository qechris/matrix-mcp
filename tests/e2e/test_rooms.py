"""Room listing and joining tests."""
import pytest

from conftest import create_room
from mcp_client import MCPError


@pytest.fixture
def alice(mcp, register_user):
    user = register_user("alice")
    server = mcp("alice")
    server.call_tool("login", {"username": user.username, "password": user.password})
    server.user = user
    return server


def _find(rooms_result, room_id):
    return next((r for r in rooms_result["rooms"] if r["room_id"] == room_id), None)


def test_list_rooms_contents(alice):
    plain = create_room(alice.user.token, "Plain Room", topic="just chatting")
    enc = create_room(alice.user.token, "Secret Room", encrypted=True)

    rooms = alice.call_tool("list_rooms")
    assert rooms["count"] >= 2

    pr = _find(rooms, plain)
    er = _find(rooms, enc)
    assert pr is not None and er is not None
    assert pr["name"] == "Plain Room"
    assert pr["topic"] == "just chatting"
    assert "Encrypt" not in (pr["encryption"] or "")
    assert er["encryption"] == "Encrypted"


def test_join_room_by_id(alice, mcp, register_user):
    bob = register_user("bob")
    room_id = create_room(bob.token, "Bob's Room", invite=[alice.user.user_id])

    joined = alice.call_tool("join_room", {"room": room_id})
    assert joined["joined"] is True
    assert joined["room_id"] == room_id

    rooms = alice.call_tool("list_rooms")
    assert _find(rooms, room_id) is not None


def test_join_room_by_alias(alice, register_user):
    bob = register_user("bob")
    alias_localpart = "bobroom-" + bob.user_id.split(":")[0].lstrip("@")[-6:]
    room_id = create_room(bob.token, "Aliased Room", alias=alias_localpart, public=True)
    alias = f"#{alias_localpart}:localhost"

    joined = alice.call_tool("join_room", {"room": alias})
    assert joined["room_id"] == room_id


def test_join_invalid_room(alice):
    with pytest.raises(MCPError):
        alice.call_tool("join_room", {"room": "not-a-valid-room-id"})
