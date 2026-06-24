# matrix-mcp end-to-end tests

A full integration suite that drives the **real `matrix-mcp` release binary**
over the MCP stdio (and SSE) transport against a **live Matrix homeserver**.
Nothing is mocked — login, sync, message round-trips, and genuine cross-device
end-to-end encryption all run against Synapse.

## Running

```sh
tests/e2e/run.sh
```

`run.sh` is self-contained. It will:

1. create a Python virtualenv and install `matrix-synapse` + `pytest`,
2. `cargo build --release`,
3. generate a throwaway Synapse config and start the homeserver (if one isn't
   already serving), and
4. run the pytest suite, then stop Synapse if it started it.

Requirements on the host: `python3` (+ `venv`), `cargo`, a C compiler, and
network access to PyPI and crates.io.

To run against an already-running homeserver, set `E2E_HOMESERVER` (and skip the
provisioning):

```sh
E2E_HOMESERVER=http://localhost:8008 \
MATRIX_MCP_BIN=$PWD/target/release/matrix-mcp \
  .work/venv/bin/pytest tests/e2e -v
```

## What's covered

| File | Coverage |
|------|----------|
| `test_protocol.py` | `initialize` serverInfo, `tools/list` shape, pre-login `whoami`, protected-tool error |
| `test_auth.py` | login success/failure, `whoami`, session persistence across a process restart |
| `test_rooms.py` | `list_rooms` contents, join by id / alias / invalid |
| `test_messaging.py` | plaintext send+read, Markdown formatting, `read_messages` limit + chronological order |
| `test_e2ee.py` | cross-device E2EE both directions, encrypted-room reporting |
| `test_transport.py` | SSE/streamable-HTTP `initialize` over HTTP |

## Layout

- `mcp_client.py` — minimal MCP stdio client used to drive the server.
- `conftest.py` — pytest fixtures (homeserver, user registration, MCP server
  factory, logged-in factory) and client-server API helpers.
