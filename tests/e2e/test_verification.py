"""Interactive device-verification tool wiring and guard rails.

The full SAS emoji round-trip needs a second device comparing emoji, so it is
verified manually against a real already-verified session rather than here.
These tests pin down the state management and error paths, which don't need a
second device: a freshly registered account has no cross-signing identity, so
starting verification fails fast rather than hanging.
"""
import pytest

from mcp_client import MCPError


def test_whoami_reports_device_verification(logged_in):
    alice = logged_in("alice")
    who = alice.call_tool("whoami")
    # A brand-new device is not cross-signed by anything yet.
    assert who["device_verified"] is False


def test_start_verification_without_cross_signing_fails(logged_in):
    alice = logged_in("alice")
    # No other session / cross-signing identity exists for a fresh account, so
    # there is nothing to verify against.
    with pytest.raises(MCPError):
        alice.call_tool("start_device_verification")


def test_continue_without_start_fails(logged_in):
    alice = logged_in("alice")
    with pytest.raises(MCPError, match="no verification in progress"):
        alice.call_tool("continue_device_verification")


def test_confirm_without_start_fails(logged_in):
    alice = logged_in("alice")
    with pytest.raises(MCPError, match="no emoji to confirm"):
        alice.call_tool("confirm_device_verification")


def test_cancel_without_start_is_noop(logged_in):
    alice = logged_in("alice")
    result = alice.call_tool("cancel_device_verification")
    assert result["cancelled"] is False
