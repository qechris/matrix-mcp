#!/usr/bin/env node
// Fills out packaging/mcpb/manifest.base.json into a full manifest.json for
// one .mcpb variant: the "universal" bundle (darwin + linux, arch-detected
// at launch by bin/launch-*.sh) or a single-platform/single-arch bundle
// (darwin-arm64, darwin-x64, linux-arm64, linux-x64) whose bin/ holds one
// plain "matrix-mcp" binary.
//
// Usage: generate-manifest.mjs --version 0.2.0 --variant universal --out path/to/manifest.json

import { readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const SCRIPT_DIR = dirname(fileURLToPath(import.meta.url));

const VARIANTS = {
  "universal": { platforms: ["darwin", "linux"], label: null },
  "darwin-arm64": { platforms: ["darwin"], label: "macOS, Apple Silicon" },
  "darwin-x64": { platforms: ["darwin"], label: "macOS, Intel" },
  "linux-arm64": { platforms: ["linux"], label: "Linux, ARM64" },
  "linux-x64": { platforms: ["linux"], label: "Linux, x86_64" },
};

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
  for (const required of ["version", "variant", "out"]) {
    if (!out[required]) throw new Error(`missing --${required}`);
  }
  if (!(out.variant in VARIANTS)) {
    throw new Error(`unknown --variant '${out.variant}'; expected one of ${Object.keys(VARIANTS).join(", ")}`);
  }
  return out;
}

const args = parseArgs(process.argv.slice(2));
const variant = VARIANTS[args.variant];
const manifest = JSON.parse(readFileSync(join(SCRIPT_DIR, "manifest.base.json"), "utf8"));

manifest.version = args.version;
manifest.compatibility = { platforms: variant.platforms };

const commonEnv = {
  MATRIX_HOMESERVER: "${user_config.homeserver}",
};

if (args.variant === "universal") {
  manifest.name = "matrix-mcp";
  manifest.display_name = "Matrix";
  manifest.server = {
    type: "binary",
    // Informational only (mcpb has no arch-level dispatch); the launcher
    // scripts pick the actual binary to run at launch time via `uname -m`.
    entry_point: "bin/matrix-mcp-linux-x64",
    mcp_config: {
      command: "${__dirname}/bin/launch-linux.sh",
      args: [],
      env: commonEnv,
      platform_overrides: {
        darwin: { command: "${__dirname}/bin/launch-darwin.sh", args: [], env: commonEnv },
        linux: { command: "${__dirname}/bin/launch-linux.sh", args: [], env: commonEnv },
      },
    },
  };
} else {
  manifest.name = `matrix-mcp-${args.variant}`;
  manifest.display_name = `Matrix (${variant.label})`;
  manifest.server = {
    type: "binary",
    entry_point: "bin/matrix-mcp",
    mcp_config: {
      command: "${__dirname}/bin/matrix-mcp",
      args: [],
      env: commonEnv,
    },
  };
}

writeFileSync(args.out, JSON.stringify(manifest, null, 2) + "\n");
