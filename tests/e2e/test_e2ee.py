"""End-to-end encryption tests: real cross-device encrypt/decrypt via the tools."""
import pytest

from conftest import create_room, read_until


@pytest.fixture
def encrypted_pair(logged_in):
    """Two logged-in users sharing an end-to-end encrypted room.

    Returns (alice, bob, room_id). Both sides are synced so device keys are
    uploaded and memberships/devices are known before any encryption happens.
    """
    alice = logged_in("alice")
    bob = logged_in("bob")
    room = create_room(alice.user.token, "Encrypted", invite=[bob.user.user_id], encrypted=True)

    alice.call_tool("sync")
    bob.call_tool("join_room", {"room": room})
    for _ in range(2):
        alice.call_tool("sync")
        bob.call_tool("sync")
    return alice, bob, room


def test_alice_encrypts_bob_decrypts(encrypted_pair):
    alice, bob, room = encrypted_pair
    alice.call_tool("send_message", {"room_id": room, "body": "secret from alice"})
    msg = read_until(bob, room, lambda m: m.get("body") == "secret from alice")
    assert isinstance(msg, dict), f"bob never decrypted alice's message: {msg}"
    assert msg["unable_to_decrypt"] is False
    assert msg["sender"] == alice.user.user_id


def test_bob_encrypts_alice_decrypts(encrypted_pair):
    alice, bob, room = encrypted_pair
    bob.call_tool("send_message", {"room_id": room, "body": "secret from bob"})
    msg = read_until(alice, room, lambda m: m.get("body") == "secret from bob")
    assert isinstance(msg, dict), f"alice never decrypted bob's message: {msg}"
    assert msg["unable_to_decrypt"] is False
    assert msg["sender"] == bob.user.user_id


def test_encrypted_room_reports_encrypted(encrypted_pair):
    alice, _bob, room = encrypted_pair
    rooms = alice.call_tool("list_rooms")
    entry = next((r for r in rooms["rooms"] if r["room_id"] == room), None)
    assert entry is not None
    assert entry["encryption"] == "Encrypted"
