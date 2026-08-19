#!/usr/bin/env node
//
// Build the agent-status-mcp MCP server and install it to a stable,
// user-level location OUTSIDE the cargo target dir:
//
//   Windows : %LOCALAPPDATA%\agent-status\bin\agent-status-mcp.exe
//   macOS   : ~/Library/Application Support/agent-status/bin/agent-status-mcp
//   Linux   : ~/.local/share/agent-status/bin/agent-status-mcp
//
// Point your MCP client config at that path instead of
// src-tauri/target/**/agent-status-mcp — a running MCP server locks its exe
// on Windows, and pointing clients at target/debug makes every `cargo build`
// fail with "Access is denied" while a client is connected.
//
// Re-running while clients hold the old exe open still works: the old file
// is renamed aside (Windows allows renaming a running exe, not overwriting)
// and the new one copied into place. Clients pick it up on next reconnect.
//
// Usage: npm run install:mcp        (release build; MCP_PROFILE=dev for debug)

import { execFileSync } from "node:child_process";
import { copyFileSync, existsSync, mkdirSync, renameSync, rmSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { homedir, platform } from "node:os";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const SRC_TAURI = join(ROOT, "src-tauri");
const isWindows = platform() === "win32";
const exeName = `agent-status-mcp${isWindows ? ".exe" : ""}`;
const profile = process.env.MCP_PROFILE === "dev" ? "dev" : "release";
const profileDir = profile === "dev" ? "debug" : "release";

console.log(`==> Building agent-status-mcp (${profile})`);
const cargoArgs = ["build", "-p", "agent-status-mcp"];
if (profile === "release") cargoArgs.push("--release");
execFileSync("cargo", cargoArgs, { cwd: SRC_TAURI, stdio: "inherit" });

const built = join(SRC_TAURI, "target", profileDir, exeName);
if (!existsSync(built)) {
  console.error(`error: expected build output not found: ${built}`);
  process.exit(1);
}

const destDir = join(installRoot(), "agent-status", "bin");
const dest = join(destDir, exeName);
mkdirSync(destDir, { recursive: true });

if (existsSync(dest)) {
  // A running exe can't be overwritten on Windows, but it can be renamed.
  const aside = `${dest}.old`;
  rmSync(aside, { force: true });
  try {
    renameSync(dest, aside);
  } catch (err) {
    console.error(`error: could not move aside ${dest}: ${err.message}`);
    process.exit(1);
  }
}
copyFileSync(built, dest);
console.log(`==> Installed: ${dest}`);
console.log("    Point your MCP client config at this path, e.g.:");
console.log(`    command = '${dest.replaceAll("/", "\\")}'`);

function installRoot() {
  if (isWindows) return process.env.LOCALAPPDATA ?? join(homedir(), "AppData", "Local");
  if (platform() === "darwin")
    return join(homedir(), "Library", "Application Support");
  return process.env.XDG_DATA_HOME ?? join(homedir(), ".local", "share");
}
