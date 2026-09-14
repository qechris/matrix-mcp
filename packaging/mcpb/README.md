# matrix-mcp Desktop Extension (.mcpb) packaging

Builds [Desktop Extension](https://github.com/anthropics/mcpb) (`.mcpb`)
bundles for one-click install in Claude Desktop, from the same release
binaries `release.yml` already produces.

Five files come out of a build: a "universal" bundle covering both macOS and
Linux (arch-detected at launch via `bin/launch-*.sh`, since the `.mcpb`
manifest format only dispatches per-OS, not per-architecture), plus four
smaller single-platform/single-arch bundles for anyone who'd rather not
download all four binaries at once.

## Layout

- `manifest.base.json` — the parts of `manifest.json` shared by every
  variant: metadata, `user_config` (homeserver/username/password, matching
  the env vars documented in the main README), and the `tools` list.
- `generate-manifest.mjs` — fills in the variant-specific parts (`name`,
  `compatibility`, `server`) and writes a complete `manifest.json`.
- `bin/launch-darwin.sh`, `bin/launch-linux.sh` — arch-detecting launchers
  used only by the universal bundle.
- `build.sh` — orchestrates the above into `.mcpb` files via the official
  [`@anthropic-ai/mcpb`](https://www.npmjs.com/package/@anthropic-ai/mcpb)
  CLI (run through `npx`, no local install needed).

## Building locally

Requires already-built release binaries (see the main
[Build](../../README.md#build) section) and Node.js.

```sh
mkdir -p /tmp/mcpb-bin
cp target/release/matrix-mcp /tmp/mcpb-bin/matrix-mcp-linux-x64   # etc. for the other 3 targets
packaging/mcpb/build.sh 0.2.0 /tmp/mcpb-bin dist-mcpb
```

`BIN_DIR` must contain exactly `matrix-mcp-darwin-arm64`,
`matrix-mcp-darwin-x64`, `matrix-mcp-linux-arm64`, and
`matrix-mcp-linux-x64` (executable). `release.yml`'s `package-mcpb` job
assembles these from the cross-platform binaries the `build` job already
produces, so in CI this step needs no cross-compilation.

## Windows

Not currently packaged: `release.yml` doesn't build a Windows binary yet.
Beyond adding a `windows-latest` target there, the default session/store
path in `matrix.rs` only checks `XDG_STATE_HOME`/`HOME`, neither of which is
reliably set on Windows — that needs fixing first. Add a `win32` platform
here once both are resolved.

## Signing

These bundles are unsigned. Claude Desktop may warn on install; see the
[mcpb `sign`/`verify` commands](https://github.com/anthropics/mcpb) if
that needs addressing.
