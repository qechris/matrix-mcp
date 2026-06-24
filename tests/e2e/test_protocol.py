"""MCP protocol-level tests: initialize, tool listing, pre-login behavior."""
import pytest

from mcp_client import MCPError

EXPECTED_TOOLS = {
    "login",
    "whoami",
    "sync",
    "list_rooms",
    "send_message",
    "read_messages",
    "join_room",
}


def test_initialize_server_info(mcp):
    server = mcp("proto")
    info = server.init_result
    assert info["serverInfo"]["name"] == "matrix-mcp"
    assert info["serverInfo"]["version"]
    assert "tools" in info["capabilities"]
    assert info["instructions"]


def test_tools_list_shape(mcp):
    server = mcp("proto")
    tools = server.list_tools()
    names = {t["name"] for t in tools}
    assert names == EXPECTED_TOOLS
    # Every tool must advertise an input schema.
    for t in tools:
        assert t.get("inputSchema", {}).get("type") == "object"
        assert t.get("description")


def test_whoami_before_login(mcp):
    server = mcp("proto")
    who = server.call_tool("whoami")
    assert who["logged_in"] is False


def test_protected_tool_requires_login(mcp):
    server = mcp("proto")
    with pytest.raises(MCPError, match="not logged in"):
        server.call_tool("list_rooms")
