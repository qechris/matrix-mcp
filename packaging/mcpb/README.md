# matrix-mcp Desktop Extension (.mcpb) packaging

Builds [Desktop Extension](https://github.com/anthropics/mcpb) (`.mcpb`)
bundles for one-click install in Claude Desktop, from the same release
binaries `release.yml` already produces.

One `matrix-mcp-VERSION.mcpb` file comes out of a build, covering both macOS
and Linux: `bin/launch-*.sh` picks the right architecture at launch, since
the `.mcpb` manifest format only dispatches per-OS, not per-architecture.

## Layout

- `manifest.base.json` — the parts of `manifest.json` that don't vary:
  metadata, `user_config` (homeserver, matching the env vars documented in
  the main README), and the `tools` list.
- `generate-manifest.mjs` — fills in `name`, `compatibility`, and `server`
  and writes a complete `manifest.json`.
- `bin/launch-darwin.sh`, `bin/launch-linux.sh` — arch-detecting launchers.
- `build.sh` — orchestrates the above into a `.mcpb` file via the official
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

## Uninstalling

Removing the extension in Claude Desktop does not remove the server's saved
login and encryption store: they live outside the extension directory, in
`$XDG_STATE_HOME/matrix-mcp/` (usually `~/.local/state/matrix-mcp/`), so
that they survive extension updates. The `.mcpb` format has no uninstall
hook to clean them up.

To remove everything, run the `logout` tool before uninstalling (it logs
the device out on the homeserver and deletes the local state), or delete
that directory afterwards. Reinstalling without doing either is safe: the
server discards state left by a device that no longer exists, and a fresh
login replaces anything left over.

## Signing

These bundles are unsigned. Claude Desktop may warn on install; see the
[mcpb `sign`/`verify` commands](https://github.com/anthropics/mcpb) if
that needs addressing.
