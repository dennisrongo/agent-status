#!/usr/bin/env node
//
// Build the agent-status-mcp MCP sidecar and stage it under src-tauri/binaries/
// with the target-triple suffix Tauri's externalBin expects:
//
//   cargo build --release -p agent-status-mcp   (in src-tauri)
//   → src-tauri/binaries/agent-status-mcp-<host-triple>[.exe]
//
// Host builds land in src-tauri/target/release (no <triple> segment — cargo
// only adds it when --target is passed explicitly). Set MCP_TARGET=<triple>
// to cross-build; then the binary is read from target/<triple>/release.

import { execFileSync } from "node:child_process";
import { copyFileSync, existsSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const SRC_TAURI = join(ROOT, "src-tauri");

const target = process.env.MCP_TARGET ?? hostTriple();
const isWindows = target.includes("windows");
const exeName = `agent-status-mcp${isWindows ? ".exe" : ""}`;

console.log(`==> Building agent-status-mcp for ${target}`);
const cargoArgs = ["build", "--release", "-p", "agent-status-mcp"];
if (process.env.MCP_TARGET) cargoArgs.push("--target", target);
execFileSync("cargo", cargoArgs, { cwd: SRC_TAURI, stdio: "inherit" });

const builtDir = process.env.MCP_TARGET
  ? join(SRC_TAURI, "target", target, "release")
  : join(SRC_TAURI, "target", "release");
const built = join(builtDir, exeName);
if (!existsSync(built)) {
  console.error(`error: expected build output not found: ${built}`);
  process.exit(1);
}

const outDir = join(SRC_TAURI, "binaries");
mkdirSync(outDir, { recursive: true });
const out = join(outDir, `agent-status-mcp-${target}${isWindows ? ".exe" : ""}`);
copyFileSync(built, out);
console.log(`==> Staged sidecar: ${out}`);

function hostTriple() {
  const out = execFileSync("rustc", ["-vV"], { encoding: "utf8" });
  const line = out.split("\n").find((l) => l.startsWith("host:"));
  if (!line) {
    console.error("error: could not parse host triple from `rustc -vV`");
    process.exit(1);
  }
  return line.slice("host:".length).trim();
}
