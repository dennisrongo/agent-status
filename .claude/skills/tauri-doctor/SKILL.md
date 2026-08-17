---
name: tauri-doctor
description: Unstick the agent-status dev build when cargo/tauri fails with "Access is denied" (Os code 5, PermissionDenied) from the Tauri build script, locked exe/sidecar binaries in src-tauri/target or src-tauri/binaries, the app exiting instantly after a good compile (exit 0xffffffff from an orphaned WebView2 holding the app's EBWebView user-data lock, Windows), "Blocking waiting for file lock on build directory", or corrupted artifacts after an interrupted build. Drives scripts/tauri-doctor.ps1 (Windows) or scripts/tauri-doctor.sh (macOS). Use this skill whenever a local dev build (`npm run tauri dev`, `cargo build`/`cargo run`/`cargo check`) fails with file-lock, permission, or instant-exit errors, or the user says the build is stuck, the sidecar is locked, or "fix the tauri/cargo errors" — even if they don't name the skill. Do NOT trigger for release builds (use release-windows/release-macos) or for genuine compile errors in the code.
---

# Tauri Doctor

Fixes the recurring local-dev build failures in this repo, via `scripts/tauri-doctor.ps1` (Windows, invoked from Git Bash as `powershell -NoProfile -ExecutionPolicy Bypass -File ./scripts/tauri-doctor.ps1`) or `scripts/tauri-doctor.sh` (macOS).

## Mental model — why the build locks up

- **Windows locks running exes.** The Tauri build script copies/links `agent-status.exe` and the MCP sidecar (`agent-status-mcp.exe`, also staged as `binaries/agent-status-mcp-x86_64-pc-windows-msvc.exe`). If any copy of those is running, the copy fails with `Os { code: 5, kind: PermissionDenied }` and the build panics in `tauri-build`. **macOS never gets this class of error** — Unix lets you replace a running binary's file.
- **The sidecar respawns.** `agent-status-mcp` processes are spawned by MCP clients (python/node MCP servers configured to use this repo's debug build), not just by the app. Killing them is safe but they come back while the client lives — expect to re-run the doctor, or stop the MCP client / disable the MCP export in app settings during heavy dev.
- **Orphaned WebViews break the next launch (Windows).** When the app is force-killed, its `msedgewebview2.exe` children can survive and hold the app's EBWebView user-data lock; the next run then compiles fine but dies instantly with `exit code: 0xffffffff`. The `-WebView` flag matches WebView processes by command line containing the app identifier (`com.dennisrongo.agentstatus`) so Edge/Teams/VS Code WebViews are never touched.
- **Cargo locks are advisory and stale-able.** A killed cargo/rustc can leave the target-dir file lock "held" by a dead process, and an interrupted build can leave half-written artifacts that fail the next link opaquely.

## When to use this skill

- `error: failed to run custom build command` + `PermissionDenied` / `Access is denied` (Os code 5)
- `process didn't exit successfully: target\debug\agent-status.exe (exit code: 0xffffffff)` right after a good compile
- `Blocking waiting for file lock on build directory` with no visible build running
- Weird link errors right after an interrupted/forced-stopped build
- User: "the build is locked again", "fix the tauri errors", "cargo errors when running the app"

Do **not** use it for Rust compile errors, type errors, or test failures — those are code problems; read the error and fix the code.

## Workflow (Windows)

1. **Default fix (do this first):**
   ```bash
   powershell -NoProfile -ExecutionPolicy Bypass -File ./scripts/tauri-doctor.ps1 -Verify
   ```
   Kills app/sidecar processes locking `src-tauri/target` or `src-tauri/binaries`, then runs `cargo check --no-default-features` to prove the build is unblocked. Safe: only touches processes whose image is one of this repo's exes or lives under the repo's `src-tauri` tree.

2. **If the app compiles but exits instantly (0xffffffff), or a run was recently force-killed:**
   ```bash
   powershell -NoProfile -ExecutionPolicy Bypass -File ./scripts/tauri-doctor.ps1 -WebView -Verify
   ```
   Stops only the orphaned `msedgewebview2.exe` processes that belong to this app (matched via the `com.dennisrongo.agentstatus` identifier in their command line), releasing the EBWebView user-data lock.

3. **If it reports a cargo file-lock wait or the lock persists** — orphaned toolchain processes:
   ```bash
   powershell -NoProfile -ExecutionPolicy Bypass -File ./scripts/tauri-doctor.ps1 -KillCargo -Verify
   ```
   Also stops every `cargo`/`rustc`/`cargo-clippy`/`rust-analyzer` on the machine (they respawn on next build). Warn the user first if they might have another Rust project building.

4. **If the next build fails with strange link/artifact errors after an interrupted build:**
   ```bash
   powershell -NoProfile -ExecutionPolicy Bypass -File ./scripts/tauri-doctor.ps1 -CleanPackages -Verify
   ```
   Runs `cargo clean -p agent-status -p agent-status-mcp` — clears only this workspace's artifacts; dependency caches survive, so the rebuild is minutes, not an hour.

5. **Preview before killing anything** — `-DryRun` prints exactly what would be stopped:
   ```bash
   powershell -NoProfile -ExecutionPolicy Bypass -File ./scripts/tauri-doctor.ps1 -DryRun -WebView -KillCargo -CleanPackages
   ```

Flags compose; typical escalation is `-Verify` → add `-WebView` / `-KillCargo` → add `-CleanPackages`. A full `cargo clean` (deps included) is the last resort and is deliberately NOT in the script — suggest it manually only after the above fails.

## Workflow (macOS)

The PermissionDenied class doesn't exist on macOS, so the bash script is a lighter equivalent:

```bash
./scripts/tauri-doctor.sh            # kill leftover app/sidecar processes (pgrep -f agent-status)
./scripts/tauri-doctor.sh --clean    # + cargo clean -p the app crates
./scripts/tauri-doctor.sh --verify   # + cargo check afterward
./scripts/tauri-doctor.sh --dry-run  # preview kills
```

Use it when a previous dev run is still alive (single-instance conflict / tray already owned) or for the same corrupted-artifact case. It has no WebView or cargo-lock steps — neither applies there.

## Anti-patterns

- **Don't `rm -rf target` as a first response.** It's a 10+ minute rebuild for what is usually a 5-second process kill.
- **Don't kill processes by PID guesswork.** The script filters by image path/name; hand-rolled `taskkill` on a PID from an old log can hit the wrong process.
- **Don't edit the script to auto-kill MCP clients** (the python/node parents). They're the user's agent sessions — killing them kills real work. Report them instead.
- **Don't use this for release builds** — `release-windows` owns that path.
- **PowerShell 5.1 reads .ps1 as ANSI without a BOM.** Keep the script pure ASCII (no em dashes / box-drawing chars) or it mis-parses — that bug has bitten once already.
