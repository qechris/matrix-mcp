"""Room listing, joining, and lifecycle (create/invite/leave/kick/ban) tests."""
import pytest

from conftest import create_room, create_room_and_sync, hs_http
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


def test_get_room_members(alice, register_user):
    bob = register_user("bob")
    room_id = create_room_and_sync(alice, "Members Room", invite=[bob.user_id])

    result = alice.call_tool("get_room_members", {"room_id": room_id})
    by_id = {m["user_id"]: m for m in result["members"]}
    assert by_id[alice.user.user_id]["membership"] == "join"
    assert by_id[bob.user_id]["membership"] == "invite"


def test_create_room_basic(alice):
    created = alice.call_tool("create_room", {"name": "New Room", "topic": "a topic"})
    assert created["created"] is True
    room_id = created["room_id"]
    assert room_id.startswith("!")

    rooms = alice.call_tool("list_rooms")
    room = _find(rooms, room_id)
    assert room is not None
    assert room["name"] == "New Room"
    assert room["topic"] == "a topic"


def test_create_room_with_invite_and_encryption(alice, register_user):
    bob = register_user("bob")
    created = alice.call_tool(
        "create_room",
        {"name": "Secret", "invite": [bob.user_id], "encrypted": True},
    )
    room_id = created["room_id"]

    members = alice.call_tool("get_room_members", {"room_id": room_id})
    by_id = {m["user_id"]: m for m in members["members"]}
    assert by_id[bob.user_id]["membership"] == "invite"

    rooms = alice.call_tool("list_rooms")
    assert _find(rooms, room_id)["encryption"] == "Encrypted"


def test_invite_user(alice, register_user):
    bob = register_user("bob")
    room_id = create_room_and_sync(alice, "Invite Room")

    invited = alice.call_tool("invite_user", {"room_id": room_id, "user_id": bob.user_id})
    assert invited["invited"] is True

    members = alice.call_tool("get_room_members", {"room_id": room_id})
    by_id = {m["user_id"]: m for m in members["members"]}
    assert by_id[bob.user_id]["membership"] == "invite"


def test_leave_room(alice):
    room_id = create_room_and_sync(alice, "Leave Room")

    left = alice.call_tool("leave_room", {"room_id": room_id})
    assert left["left"] is True

    rooms = alice.call_tool("list_rooms")
    assert _find(rooms, room_id) is None


def _membership_state(token, room_id, user_id):
    status, resp = hs_http(
        "GET", f"/_matrix/client/v3/rooms/{room_id}/state/m.room.member/{user_id}", token=token
    )
    assert status == 200, resp
    return resp["membership"]


def test_kick_room_member(alice, mcp, register_user):
    bob = register_user("bob")
    room_id = create_room_and_sync(alice, "Kick Room", invite=[bob.user_id])
    bob_server = mcp("bob")
    bob_server.call_tool("login", {"username": bob.username, "password": bob.password})
    bob_server.call_tool("join_room", {"room": room_id})

    kicked = alice.call_tool(
        "kick_room_member", {"room_id": room_id, "user_id": bob.user_id, "reason": "testing"}
    )
    assert kicked["kicked"] is True
    assert _membership_state(alice.user.token, room_id, bob.user_id) == "leave"


def test_ban_and_unban_room_member(alice, mcp, register_user):
    bob = register_user("bob")
    room_id = create_room_and_sync(alice, "Ban Room", invite=[bob.user_id])
    bob_server = mcp("bob")
    bob_server.call_tool("login", {"username": bob.username, "password": bob.password})
    bob_server.call_tool("join_room", {"room": room_id})

    banned = alice.call_tool("ban_room_member", {"room_id": room_id, "user_id": bob.user_id})
    assert banned["banned"] is True
    assert _membership_state(alice.user.token, room_id, bob.user_id) == "ban"

    unbanned = alice.call_tool("unban_room_member", {"room_id": room_id, "user_id": bob.user_id})
    assert unbanned["unbanned"] is True
    assert _membership_state(alice.user.token, room_id, bob.user_id) == "leave"


def test_update_room_name_and_topic(alice):
    room_id = create_room_and_sync(alice, "Old Name")

    updated = alice.call_tool(
        "update_room", {"room_id": room_id, "name": "New Name", "topic": "New Topic"}
    )
    assert updated["updated"] is True

    rooms = alice.call_tool("list_rooms")
    room = _find(rooms, room_id)
    assert room["name"] == "New Name"
    assert room["topic"] == "New Topic"
