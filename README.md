# matrix-mcp

[![CI](https://github.com/qechris/matrix-mcp/actions/workflows/ci.yml/badge.svg)](https://github.com/qechris/matrix-mcp/actions/workflows/ci.yml)
[![Release](https://github.com/qechris/matrix-mcp/actions/workflows/release.yml/badge.svg)](https://github.com/qechris/matrix-mcp/actions/workflows/release.yml)
[![Docker](https://github.com/qechris/matrix-mcp/actions/workflows/docker.yml/badge.svg)](https://github.com/qechris/matrix-mcp/actions/workflows/docker.yml)
[![GHCR](https://img.shields.io/badge/ghcr.io-qechris%2Fmatrix--mcp-blue?logo=docker)](https://github.com/qechris/matrix-mcp/pkgs/container/matrix-mcp)

A [Model Context Protocol](https://modelcontextprotocol.io) (MCP) server for the
[Matrix](https://matrix.org) chat protocol, written in Rust on top of the two
official SDKs:

- [`rmcp`](https://crates.io/crates/rmcp) — the official MCP Rust SDK (server +
  stdio transport + `#[tool]` macros)
- [`matrix-sdk`](https://crates.io/crates/matrix-sdk) — the official Matrix Rust
  SDK

It lets any MCP-capable assistant log in to a
Matrix homeserver and drive most of what a full chat client can do: manage
rooms and their membership, send and edit messages, react and reply, exchange
files and images, look up profiles, and hold direct-message conversations.

## How to use

A quick path from install to chatting through your MCP-capable assistant,
with this device verified for encrypted rooms.

### 1. Get the binary

**Claude Desktop (easiest):** download a `.mcpb` file from the
[latest release](https://github.com/qechris/matrix-mcp/releases/latest) and
double-click it to install — this handles step 2 for you too. See
[packaging/mcpb](packaging/mcpb) for which file to pick.

**Prebuilt (recommended for everything else):** download the archive for
your platform (`x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`,
`x86_64-apple-darwin`, or `aarch64-apple-darwin`) from the
[latest release](https://github.com/qechris/matrix-mcp/releases/latest),
along with its `.sha256` file, then verify and extract:

```sh
sha256sum -c matrix-mcp-*.sha256    # macOS: shasum -a 256 -c matrix-mcp-*.sha256
tar -xzf matrix-mcp-*.tar.gz
```

This unpacks a `matrix-mcp` binary alongside `README.md` and `LICENSE`.

**From source:** `cargo build --release` — see [Build](#build).

**Container image:** `ghcr.io/qechris/matrix-mcp` — see
[Container image](#container-image).

### 2. Register it with your MCP client

The server speaks MCP over stdio. Add an entry to your MCP client's server
configuration:

```json
{
  "mcpServers": {
    "matrix": {
      "command": "/path/to/matrix-mcp",
      "env": {
        "MATRIX_HOMESERVER": "https://matrix.org"
      }
    }
  }
}
```

You can bake `MATRIX_USER`/`MATRIX_PASSWORD` into `env` for automatic login at
startup (see [Configuration](#configuration)), or skip that and log in
interactively in the next step instead.

### 3. Log in

Ask your assistant to run `whoami`. If it reports `"logged_in": false`, log in:

- **Normal username/password homeserver:** ask it to run `login` with your
  username and password.
- **SSO/OAuth-only homeserver:** ask it to run `login_sso` instead — it opens
  your browser to finish signing in and completes automatically. See
  [SSO / OAuth-only homeservers](#sso--oauth-only-homeservers) for headless
  alternatives.

The session is saved to disk, so you won't need to log in again on the next
start.

### 4. Try the basics

With a logged-in session, just ask your assistant in plain language — it maps
these to tool calls:

- "List my rooms" → `list_rooms`
- "Send 'hello' to #general:matrix.org" → `send_message`
- "What's been said in that room recently?" → `read_messages`
- "Catch me up on that thread" → `read_thread`
- "React to that with a thumbs up" → `send_reaction`
- "Start a DM with @bob:matrix.org" → `create_dm`

### 5. Verifying this device to unlock encrypted history

A brand-new device can send and receive encrypted messages right away, but it
starts out **unverified**: other users' clients will flag it as untrusted, and
it can't read messages sent before it existed. Fix both by verifying it
against another session you already trust (e.g. Element), using emoji/SAS
verification — no recovery key needed:

1. Ask your assistant to run `start_device_verification`. This sends a
   verification request to your other, already-verified session and waits
   (up to `MATRIX_VERIFICATION_TIMEOUT` seconds, default 35) for emoji to
   compare.
2. Open that other session (e.g. Element) and accept the incoming
   verification request.
3. If `start_device_verification` returned a `"pending"` status because the
   other session hadn't accepted in time, ask your assistant to run
   `continue_device_verification` — repeat until it returns emoji.
4. Compare the emoji shown here against the emoji shown in the other session.
   - **They match:** run `confirm_device_verification`. This device is now
     cross-signed, and the other device gossips it the cross-signing secrets
     and key-backup key automatically — no recovery key to type.
   - **They don't match:** run `cancel_device_verification` and investigate;
     don't confirm.

This requires the account to already have a cross-signing identity (set one
up in another client first if it doesn't) and only verifies this device
against *your own* other sessions — see [Limitations](#limitations).

If you'd rather not do the interactive dance (e.g. a headless deployment),
`restore_key_backup` with a recovery key achieves the "read old history" half
without needing another session online — see
[Reading messages sent before this device existed](#reading-messages-sent-before-this-device-existed).

## Tools

| Tool            | Description |
|-----------------|-------------|
| `login`         | Log in with username/password. The session is persisted and reused on the next start. |
| `login_sso`     | Log in via the homeserver's SSO flow: opens a browser, waits for you to finish signing in. |
| `login_with_token` | Log in with a pre-obtained access token, for headless/automated setups on an SSO/OAuth homeserver. |
| `logout`        | Log out, invalidate the access token, and clear the saved session. |
| `whoami`        | Report the current login state (user id, device, homeserver, joined-room count, key-backup state). |
| `sync`          | Run a single sync to refresh the local room list and state, and flush pending key-backup uploads. |
| `enable_key_backup` | Set up a server-side key backup and return a new recovery key (for accounts that don't have one). |
| `restore_key_backup` | Unlock the key backup with a recovery key, making messages sent before this device existed readable. |
| `download_room_keys` | Force-fetch a room's historical keys from the key backup. |
| `start_device_verification` | Start interactive (emoji/SAS) verification of this device against your other, already-verified session. Returns emoji to compare, once ready. |
| `continue_device_verification` | Continue an in-progress device verification, fetching the emoji once the other session has accepted. |
| `confirm_device_verification` | Confirm the emoji match, completing device verification and cross-signing. |
| `cancel_device_verification` | Abort an in-progress device verification. |
| `list_rooms`    | List joined rooms with id, name, topic, and encryption state. |
| `send_message`  | Send a text message to a room (plain text or Markdown), optionally as a rich reply — replies to a message in a thread stay in that thread. |
| `edit_message`  | Edit a previously-sent message (sender only). |
| `redact_event`  | Redact (delete) a message, or a reaction to un-react. |
| `send_reaction` | React to a message with an emoji. |
| `mark_read`     | Mark a room as read up to a given event, or the latest message. |
| `read_messages` | Read messages from a room, in chronological order, with pagination via `before_token`/`next_token`. |
| `read_thread`   | Read one conversation thread — the root message plus its replies, oldest first — from the root's event id or any reply in it. |
| `get_room_members` | List a room's members with display name, membership state, and power level. |
| `join_room`     | Join a room by id (`!room:server`) or alias (`#room:server`). |
| `create_room`   | Create a room, optionally with a name, topic, invites, public visibility, encryption, or as a DM. |
| `invite_user`   | Invite a user to a room. |
| `leave_room`    | Leave a room. |
| `kick_room_member` | Kick a member from a room, optionally with a reason. |
| `ban_room_member`  | Ban a member from a room, optionally with a reason. |
| `unban_room_member`| Unban a previously-banned member. |
| `update_room`   | Update a room's name and/or topic. |
| `create_dm`     | Get or create a direct-message room with a user. |
| `get_profile`   | Look up a user's display name and avatar (defaults to the logged-in user). |
| `send_file`     | Upload a local file and send it as an image, audio, video, or generic attachment. |
| `download_media`| Download the media attached to a message event to a local path. |

## Configuration

The server is configured through environment variables. All are optional — you
can also authenticate at runtime with the `login` tool.

| Variable             | Description |
|----------------------|-------------|
| `MATRIX_HOMESERVER`  | Default homeserver URL, e.g. `https://matrix.org`. |
| `MATRIX_USER`        | Username for automatic login at startup. |
| `MATRIX_PASSWORD`    | Password for automatic login at startup. |
| `MATRIX_ACCESS_TOKEN`| Pre-obtained access token for automatic login at startup, for SSO/OAuth-only homeservers (see below). Requires `MATRIX_USER_ID` and `MATRIX_DEVICE_ID` too. |
| `MATRIX_USER_ID`     | Full user id (e.g. `@alice:matrix.org`) matching `MATRIX_ACCESS_TOKEN`. |
| `MATRIX_DEVICE_ID`   | Device id the access token was issued for. |
| `MATRIX_DEVICE_NAME` | Device display name (default `matrix-mcp`), used only for password login. |
| `MATRIX_SESSION_FILE`| Path to persist the session (default: `$XDG_STATE_HOME/matrix-mcp/session.json`, falling back to `~/.local/state/matrix-mcp/session.json`). |
| `MATRIX_STORE_PATH`  | Directory for the SQLite crypto/state store, where E2EE keys and room state persist (default: a `store` directory next to the session file). |
| `MATRIX_VERIFICATION_TIMEOUT` | Seconds each interactive device-verification poll (`start_device_verification`/`continue_device_verification`) waits for the other session before returning `pending` (default `35`). |
| `MATRIX_MCP_TRANSPORT` | Transport to serve: `stdio` (default) or `http`/`sse`. |
| `MATRIX_MCP_ADDRESS` | Bind address for the HTTP/SSE transport (default `127.0.0.1:8000`). |
| `MATRIX_MCP_PATH`    | URL path for the HTTP/SSE endpoint (default `/mcp`). |
| `RUST_LOG`           | Log filter, e.g. `matrix_mcp=debug,matrix_sdk=info`. Logs go to stderr. |

On startup the server tries to restore a saved session; if none exists, it
performs a password login when `MATRIX_USER`/`MATRIX_PASSWORD` are set, or an
access-token login when `MATRIX_ACCESS_TOKEN`/`MATRIX_USER_ID`/`MATRIX_DEVICE_ID`
are set (password login takes priority if both are configured).

### SSO / OAuth-only homeservers

`login` (username + password) doesn't work against a homeserver that requires
SSO for interactive login. There are two ways to authenticate against one
instead:

- **`login_sso`, for a human at the keyboard.** It opens a local callback
  listener on `localhost` (a random port in `20000..30000`), asks the
  homeserver for an SSO login URL, and opens that URL in your default browser
  (best-effort — if it can't, the URL is returned in the result so you can
  open it yourself). The tool call blocks for up to 5 minutes while you
  complete the sign-in in the browser; once you do, the homeserver redirects
  back to the local listener with a one-time token that's exchanged for a
  real session — server-issued, on a fresh device id, no manual token-copying
  involved. This is the easiest path for onboarding people onto a shared
  matrix-mcp deployment: each person just runs `login_sso` and signs in with
  their own SSO/IdP flow like they normally would.
- **`login_with_token` (or the `MATRIX_ACCESS_TOKEN` env vars), for headless
  or automated setups** where nothing can open a browser at all (e.g. a
  server-side deployment with no display) — supply a token obtained some
  other way:
  - **Preferred: mint one via your homeserver's admin API**, if you have
    admin access (e.g. Synapse's `POST /_synapse/admin/v1/users/<user_id>/login`).
    This creates a fresh device too, so it won't collide with any client
    you're already using.
  - **Or copy one out of an already-logged-in client** (e.g. in Element web:
    Settings → Help & About → Advanced → Access Token). This works, but the
    token is tied to that client's existing device id — reusing it here means
    matrix-mcp and that client share one device identity, which will cause
    encryption state conflicts in encrypted rooms. Fine for unencrypted rooms
    or short-lived use; avoid it as a long-term setup.

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
- Interactive device verification (emoji/SAS, via cross-signing) is supported —
  see [Verifying this device](#verifying-this-device-to-unlock-encrypted-history)
  below. Until this device is verified, other users' clients may flag it as
  unverified, and it can't be gossiped historical room keys.

### Reading messages sent before this device existed

A freshly logged-in device holds none of the room keys for older messages, so
history in encrypted rooms comes back with `unable_to_decrypt: true`. Those keys
live in the account's **server-side key backup**, encrypted with a *recovery key*
(also called a security key — the `EsTx xxxx …` string — or a security
passphrase). Unlock it once per device:

```
restore_key_backup(recovery_key: "EsTx xxxx xxxx …")
```

After that, `read_messages` pulls a room's historical keys automatically the
first time it hits an undecryptable message and re-decodes the page, so old
messages simply appear. `download_room_keys` forces the same fetch manually, and
`whoami` reports the current `key_backup` state.

If the account has no backup yet, `enable_key_backup` creates one and returns a
newly generated recovery key. **Save it** — it is shown once, cannot be
recovered, and without it messages in encrypted rooms are unreadable on any
future device. It refuses to run when a backup already exists, since creating a
second one would invalidate the existing recovery key and strand the keys backed
up under it.

Note that no recovery key can retroactively rescue messages whose keys were
never backed up — if key backup was never enabled on the account, that history
is gone for any new device. `sync` flushes this device's pending key uploads to
the backup so its own messages don't end up in that state.

## Container image

Multi-arch images (`linux/amd64`, `linux/arm64`) are published to the GitHub
Container Registry on every push to `main` (tagged `main` / `edge`) and on every
release tag (`X.Y.Z`, `X.Y`, and `latest` for non-prereleases):

```sh
docker run --rm -p 8000:8000 -v matrix-mcp-data:/data \
  -e MATRIX_HOMESERVER=https://matrix.org \
  ghcr.io/qechris/matrix-mcp:latest
# SSE/streamable-HTTP MCP endpoint: http://localhost:8000/mcp
```

The image defaults to the SSE transport bound to `0.0.0.0:8000`, runs as a
non-root user, and persists the session + encryption store under the `/data`
volume. The endpoint has no auth of its own — front it with a reverse proxy
(auth + TLS) before exposing it to untrusted networks.

**Available tags:** pushes to `main` produce `:main` and `:edge`; the `:latest`
(and `:X.Y` / `:X.Y.Z`) tags are only created by a release tag (`vX.Y.Z`), so
until the first release, pull `:main`.

**Visibility:** GHCR packages are **private by default**, even for a public
repo. To allow anonymous `docker pull`, make the package public once at
`https://github.com/users/qechris/packages/container/matrix-mcp/settings`
(Danger Zone → Change visibility → Public). Otherwise authenticate first:

```sh
echo "$GHCR_TOKEN" | docker login ghcr.io -u qechris --password-stdin   # token needs read:packages
docker pull ghcr.io/qechris/matrix-mcp:main
```

## Continuous integration & deployment

GitHub Actions workflows under `.github/workflows/`:

| Workflow | Trigger | What it does |
|----------|---------|--------------|
| `ci.yml` | PRs, push to `main` | `rustfmt`, `clippy -D warnings`, build/test, and the full E2E suite (`tests/e2e/run.sh`) against a real Synapse |
| `audit.yml` | Cargo.lock changes, weekly | `cargo audit` against the RustSec advisory DB |
| `release.yml` | tags `v*.*.*` | builds native Linux binaries (x86_64 + aarch64), publishes a GitHub Release with checksums, and publishes to crates.io if `CARGO_REGISTRY_TOKEN` is set |
| `docker.yml` | push to `main`, tags `v*.*.*` | builds and pushes the multi-arch (`amd64` + `arm64`) image to GHCR |

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

- Room history is fetched on demand rather than cached into a local timeline
  (`read_messages` supports paging further back via `before_token`/`next_token`,
  but there's no persistent local cache across calls).
- Device verification is self-verification only (this device against your own
  other, already cross-signed sessions). It requires the account to already
  have a cross-signing identity — set one up in another client (e.g. Element)
  first — and it can't verify other users' devices.
- Threads can be read (`read_thread`) and replied into — replying to a message
  that's already in a thread keeps the reply in that thread — but not *started*:
  replying to a thread's root, or to any other unthreaded message, sends an
  ordinary rich reply in the room timeline.
- `send_file`/`download_media` read from and write to the local filesystem the
  server process runs on, not the MCP client's.

## License

MIT
