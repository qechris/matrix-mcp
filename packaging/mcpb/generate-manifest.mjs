#!/usr/bin/env node
// Fills out packaging/mcpb/manifest.base.json into a complete manifest.json
// for the matrix-mcp Desktop Extension: one bundle covering macOS and Linux
// (Windows isn't built yet), with bin/launch-*.sh picking the right
// architecture at launch since the .mcpb manifest format only dispatches
// per-OS, not per-architecture.
//
// Usage: generate-manifest.mjs --version 0.2.0 --out path/to/manifest.json

import { readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const SCRIPT_DIR = dirname(fileURLToPath(import.meta.url));

function parseArgs(argv) {
  const out = {};
  for (let i = 0; i < argv.length; i += 2) {
    const key = argv[i]?.replace(/^--/, "");
    const value = argv[i + 1];
    if (!key || value === undefined) {
      throw new Error(`bad argument pair at index ${i}: ${argv[i]} ${argv[i + 1]}`);
    }
    out[key] = value;
  }
  for (const required of ["version", "out"]) {
    if (!out[required]) throw new Error(`missing --${required}`);
  }
  return out;
}

const args = parseArgs(process.argv.slice(2));
const manifest = JSON.parse(readFileSync(join(SCRIPT_DIR, "manifest.base.json"), "utf8"));

manifest.version = args.version;
manifest.name = "matrix-mcp";
manifest.display_name = "Matrix";
manifest.compatibility = { platforms: ["darwin", "linux"] };

const env = { MATRIX_HOMESERVER: "${user_config.homeserver}" };
manifest.server = {
  type: "binary",
  // Informational only (mcpb has no arch-level dispatch); the launcher
  // scripts pick the actual binary to run at launch time via `uname -m`.
  entry_point: "bin/matrix-mcp-linux-x64",
  mcp_config: {
    command: "${__dirname}/bin/launch-linux.sh",
    args: [],
    env,
    platform_overrides: {
      darwin: { command: "${__dirname}/bin/launch-darwin.sh", args: [], env },
      linux: { command: "${__dirname}/bin/launch-linux.sh", args: [], env },
    },
  },
};

writeFileSync(args.out, JSON.stringify(manifest, null, 2) + "\n");
