"""Direct-message room creation/reuse and user profile lookup tests."""
import pytest

from conftest import hs_http


@pytest.fixture
def alice(logged_in):
    return logged_in("alice")


def test_create_dm_creates_room_and_invites(alice, register_user):
    bob = register_user("bob")

    created = alice.call_tool("create_dm", {"user_id": bob.user_id})
    room_id = created["room_id"]
    assert room_id.startswith("!")

    members = alice.call_tool("get_room_members", {"room_id": room_id})
    by_id = {m["user_id"]: m for m in members["members"]}
    assert by_id[bob.user_id]["membership"] == "invite"


def test_create_dm_reuses_existing_room(alice, register_user):
    bob = register_user("bob")

    first = alice.call_tool("create_dm", {"user_id": bob.user_id})
    second = alice.call_tool("create_dm", {"user_id": bob.user_id})
    assert first["room_id"] == second["room_id"]


def test_get_profile_defaults_to_self(alice):
    profile = alice.call_tool("get_profile")
    assert profile["user_id"] == alice.user.user_id


def test_get_profile_reads_display_name(alice, register_user):
    bob = register_user("bob")
    status, resp = hs_http(
        "PUT",
        f"/_matrix/client/v3/profile/{bob.user_id}/displayname",
        token=bob.token,
        body={"displayname": "Bob the Builder"},
    )
    assert status == 200, resp

    profile = alice.call_tool("get_profile", {"user_id": bob.user_id})
    assert profile["user_id"] == bob.user_id
    assert profile["display_name"] == "Bob the Builder"
