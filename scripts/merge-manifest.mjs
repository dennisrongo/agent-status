#!/usr/bin/env node
//
// Merge ONE platform's updater entry into the shared `updater/latest.json`
// manifest, without clobbering the other platforms' signatures.
//
// agent-status ships from two machines (a Mac and a Windows PC) but the Tauri
// updater reads a SINGLE latest.json per GitHub release. Each machine signs its
// own payload with the SAME updater private key (the app verifies every
// platform against the one pubkey in tauri.conf.json), then calls this helper
// to splice its entry in. Whoever builds second reads the committed manifest,
// sees the same version, and KEEPS the first machine's signatures.
//
// Version is the gate:
//   - committed version === build version  -> keep existing platforms, add/replace ours
//   - committed version !== build version  -> start fresh (drop stale prior-version entries)
//
// So a Windows build for v0.3.0 preserves the Mac's darwin-* signatures for
// v0.3.0, but a Windows build for v0.4.0 will NOT carry a stale v0.3.0 darwin
// entry forward (which would tell darwin installs to "update" to an old payload).
//
// Usage:
//   node scripts/merge-manifest.mjs \
//     --manifest updater/latest.json \
//     --version 0.3.0 \
//     --platforms darwin-aarch64,darwin-x86_64 \
//     --sig-file "path/to/Agent Usage Monitor.app.tar.gz.sig" \
//     --url "https://github.com/.../v0.3.0/Agent.Usage.Monitor.app.tar.gz" \
//     [--notes "Agent Usage Monitor 0.3.0"]
//
// --sig <base64 string> may be used instead of --sig-file.
// All listed --platforms share the one signature + url (e.g. a universal mac
// payload satisfies both darwin arches).

import { readFileSync, writeFileSync, existsSync, mkdirSync } from "node:fs";
import { dirname } from "node:path";

function parseArgs(argv) {
  const out = {};
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a.startsWith("--")) {
      const key = a.slice(2);
      const next = argv[i + 1];
      if (next === undefined || next.startsWith("--")) {
        out[key] = true;
      } else {
        out[key] = next;
        i++;
      }
    }
  }
  return out;
}

function die(msg) {
  console.error(`merge-manifest: ${msg}`);
  process.exit(1);
}

const args = parseArgs(process.argv.slice(2));

const manifestPath = args.manifest || "updater/latest.json";
const version = args.version;
const platformsArg = args.platforms;
const url = args.url;
const notes = args.notes || (version ? `Agent Usage Monitor ${version}` : undefined);

if (!version) die("--version is required");
if (!platformsArg) die("--platforms is required (comma-separated, e.g. windows-x86_64)");
if (!url) die("--url is required");

let signature = args.sig;
if (args["sig-file"]) {
  if (!existsSync(args["sig-file"])) die(`--sig-file not found: ${args["sig-file"]}`);
  signature = readFileSync(args["sig-file"], "utf8").trim();
}
if (!signature) die("provide --sig-file <path> or --sig <string>");

const platforms = platformsArg.split(",").map((p) => p.trim()).filter(Boolean);
if (platforms.length === 0) die("no platforms parsed from --platforms");

// Load the existing manifest if present; reset its platforms when the version
// differs so we never carry a stale prior-version entry forward.
let manifest = { version, notes, pub_date: "", platforms: {} };
if (existsSync(manifestPath)) {
  try {
    const existing = JSON.parse(readFileSync(manifestPath, "utf8"));
    if (existing && existing.version === version && existing.platforms) {
      manifest = existing;
      manifest.notes = notes; // keep notes current
    } else {
      console.error(
        `merge-manifest: existing manifest is v${existing?.version ?? "?"}, building v${version} — starting a fresh manifest (dropping stale entries).`,
      );
    }
  } catch (e) {
    console.error(`merge-manifest: could not parse ${manifestPath} (${e.message}); starting fresh.`);
  }
}

manifest.version = version;
manifest.notes = notes;
manifest.pub_date = new Date().toISOString().replace(/\.\d{3}Z$/, "Z");
manifest.platforms = manifest.platforms || {};

for (const p of platforms) {
  manifest.platforms[p] = { signature, url };
}

mkdirSync(dirname(manifestPath), { recursive: true });
writeFileSync(manifestPath, JSON.stringify(manifest, null, 2) + "\n", "utf8");

console.log(
  `merge-manifest: ${manifestPath} -> v${version}, platforms: ${Object.keys(manifest.platforms).sort().join(", ")}`,
);
