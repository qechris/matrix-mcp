# matrix-mcp

[![CI](https://github.com/qa-chrisb/matrix-mcp/actions/workflows/ci.yml/badge.svg)](https://github.com/qa-chrisb/matrix-mcp/actions/workflows/ci.yml)
[![Release](https://github.com/qa-chrisb/matrix-mcp/actions/workflows/release.yml/badge.svg)](https://github.com/qa-chrisb/matrix-mcp/actions/workflows/release.yml)
[![Docker](https://github.com/qa-chrisb/matrix-mcp/actions/workflows/docker.yml/badge.svg)](https://github.com/qa-chrisb/matrix-mcp/actions/workflows/docker.yml)
[![GHCR](https://img.shields.io/badge/ghcr.io-qa--chrisb%2Fmatrix--mcp-blue?logo=docker)](https://github.com/qa-chrisb/matrix-mcp/pkgs/container/matrix-mcp)

A [Model Context Protocol](https://modelcontextprotocol.io) (MCP) server for the
[Matrix](https://matrix.org) chat protocol, written in Rust on top of the two
official SDKs:

- [`rmcp`](https://crates.io/crates/rmcp) — the official MCP Rust SDK (server +
  stdio transport + `#[tool]` macros)
- [`matrix-sdk`](https://crates.io/crates/matrix-sdk) — the official Matrix Rust
  SDK

It lets an MCP client (Claude, or any other MCP-capable assistant) log in to a
Matrix homeserver, list rooms, read and send messages, and join rooms.

## Tools

| Tool            | Description |
|-----------------|-------------|
| `login`         | Log in with username/password. The session is persisted and reused on the next start. |
| `whoami`        | Report the current login state (user id, device, homeserver, joined-room count). |
| `sync`          | Run a single sync to refresh the local room list and state. |
| `list_rooms`    | List joined rooms with id, name, topic, and encryption state. |
| `send_message`  | Send a text message to a room (plain text or Markdown). |
| `read_messages` | Read the most recent messages from a room, in chronological order. |
| `join_room`     | Join a room by id (`!room:server`) or alias (`#room:server`). |

## Configuration

The server is configured through environment variables. All are optional — you
can also authenticate at runtime with the `login` tool.

| Variable             | Description |
|----------------------|-------------|
| `MATRIX_HOMESERVER`  | Default homeserver URL, e.g. `https://matrix.org`. |
| `MATRIX_USER`        | Username for automatic login at startup. |
| `MATRIX_PASSWORD`    | Password for automatic login at startup. |
| `MATRIX_DEVICE_NAME` | Device display name (default `matrix-mcp`). |
| `MATRIX_SESSION_FILE`| Path to persist the session (default: `$XDG_STATE_HOME/matrix-mcp/session.json`, falling back to `~/.local/state/matrix-mcp/session.json`). |
| `MATRIX_STORE_PATH`  | Directory for the SQLite crypto/state store, where E2EE keys and room state persist (default: a `store` directory next to the session file). |
| `MATRIX_MCP_TRANSPORT` | Transport to serve: `stdio` (default) or `http`/`sse`. |
| `MATRIX_MCP_ADDRESS` | Bind address for the HTTP/SSE transport (default `127.0.0.1:8000`). |
| `MATRIX_MCP_PATH`    | URL path for the HTTP/SSE endpoint (default `/mcp`). |
| `RUST_LOG`           | Log filter, e.g. `matrix_mcp=debug,matrix_sdk=info`. Logs go to stderr. |

On startup the server tries to restore a saved session; if none exists and
`MATRIX_USER`/`MATRIX_PASSWORD` are set, it performs a password login.

## Transports

The server supports two MCP transports, selected with `MATRIX_MCP_TRANSPORT`:

- **`stdio`** (default) — the classic stdio transport for local MCP clients.
- **`http`** / **`sse`** — the SSE-based [streamable-HTTP](https://modelcontextprotocol.io/specification/2025-06-18/basic/transports#streamable-http)
  transport, served over a TCP socket for remote/networked clients. Server →
  client messages are streamed as Server-Sent Events on the same endpoint.

Run the SSE/HTTP transport:

```sh
MATRIX_MCP_TRANSPORT=sse \
MATRIX_MCP_ADDRESS=127.0.0.1:8000 \
MATRIX_MCP_PATH=/mcp \
  ./target/release/matrix-mcp
# MCP endpoint: http://127.0.0.1:8000/mcp
```

## Build

```sh
cargo build --release
```

The binary is written to `target/release/matrix-mcp`. `matrix-sdk` is built with
rustls (no OpenSSL), end-to-end encryption, and a bundled SQLite store (compiled
from source, so a C compiler is required for the build).

## Use with an MCP client

The server speaks MCP over stdio. Register it with your client, for example for
Claude Code:

```sh
claude mcp add matrix \
  --env MATRIX_HOMESERVER=https://matrix.org \
  --env MATRIX_USER=alice \
  --env MATRIX_PASSWORD=... \
  -- /path/to/target/release/matrix-mcp
```

Or, equivalently, an entry in an MCP `servers` configuration:

```json
{
  "mcpServers": {
    "matrix": {
      "command": "/path/to/target/release/matrix-mcp",
      "env": {
        "MATRIX_HOMESERVER": "https://matrix.org",
        "MATRIX_USER": "alice",
        "MATRIX_PASSWORD": "..."
      }
    }
  }
}
```

## End-to-end encryption

E2EE is enabled. The server encrypts outgoing messages in encrypted rooms and
decrypts incoming ones automatically, using a persistent SQLite crypto store
(`MATRIX_STORE_PATH`) so device and room keys survive restarts.

A few practical notes:

- The store directory holds your encryption keys — treat it like a credential
  and keep it private.
- You can only decrypt messages for which the device has the keys. Running the
  `sync` tool lets the device receive room keys (and `automatic-room-key-forwarding`
  requests missing ones); messages with no available key are returned with
  `unable_to_decrypt: true`.
- This build does not perform interactive device verification or cross-signing,
  so other users may see this device as unverified.

## Container image

A `linux/amd64` image is published to the GitHub Container Registry on every
push to `main` (tagged `main` / `edge`) and on every release tag (`X.Y.Z`,
`X.Y`, and `latest` for non-prereleases):

```sh
docker run --rm -p 8000:8000 -v matrix-mcp-data:/data \
  -e MATRIX_HOMESERVER=https://matrix.org \
  ghcr.io/qa-chrisb/matrix-mcp:latest
# SSE/streamable-HTTP MCP endpoint: http://localhost:8000/mcp
```

The image defaults to the SSE transport bound to `0.0.0.0:8000`, runs as a
non-root user, and persists the session + encryption store under the `/data`
volume. The endpoint has no auth of its own — front it with a reverse proxy
(auth + TLS) before exposing it to untrusted networks.

## Continuous integration & deployment

GitHub Actions workflows under `.github/workflows/`:

| Workflow | Trigger | What it does |
|----------|---------|--------------|
| `ci.yml` | PRs, push to `main` | `rustfmt`, `clippy -D warnings`, build/test, and the full E2E suite (`tests/e2e/run.sh`) against a real Synapse |
| `audit.yml` | Cargo.lock changes, weekly | `cargo audit` against the RustSec advisory DB |
| `release.yml` | tags `v*.*.*` | builds native binaries (Linux x86_64, macOS x86_64 + Apple Silicon, Windows x86_64), publishes a GitHub Release with checksums, and publishes to crates.io if `CARGO_REGISTRY_TOKEN` is set |
| `docker.yml` | push to `main`, tags `v*.*.*` | builds and pushes the `linux/amd64` image to GHCR |

Dependency updates are managed by Dependabot (`.github/dependabot.yml`).

## Releasing

1. Bump `version` in `Cargo.toml` (the release build fails if the tag doesn't
   match) and commit.
2. Tag and push:

   ```sh
   git tag v0.1.0
   git push origin v0.1.0
   ```

3. `release.yml` builds the cross-platform binaries and publishes the GitHub
   Release; `docker.yml` builds and pushes the container image. Use a
   `vX.Y.Z-rc1`-style tag for a prerelease (marked prerelease; not tagged
   `latest`).

To enable crates.io publishing, add a `CARGO_REGISTRY_TOKEN` repository secret;
without it the publish step is skipped cleanly.

## Limitations

- Room history is fetched on demand rather than cached into a local timeline.
- No interactive device verification / cross-signing (see above).

## License

MIT
