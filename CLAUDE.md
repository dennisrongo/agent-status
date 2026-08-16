# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

A macOS (and Windows) **menubar widget** built with Tauri 2 that tracks AI coding agent usage across multiple providers (Anthropic, Z.ai, GitHub, Alibaba, Moonshot, xAI). It reads local CLI session logs, optionally fetches live vendor quota data, and renders limits, token spend, cost estimates, and per-session history in a click-to-toggle dropdown window. Menubar-only (`LSUIElement` / `skipTaskbar`), single-instance, launch-at-login, self-updating via the Tauri updater.

## Commands

```bash
npm install                 # NOTE: if your shell has NODE_ENV=production, use
                            #   NODE_ENV=development npm install --include=dev
npm run tauri dev           # run the app (Vite + Rust, hot reload)
npm run tauri build         # bundle an UNSIGNED .app + .dmg (local testing only)
npm run build               # frontend only: tsc typecheck + vite build → dist/

cd src-tauri && cargo test --all          # full Rust suite (scanner, encryption, vendor parsers)
cd src-tauri && cargo test scan_          # one module/group by name substring
cd src-tauri && cargo test --lib status_for   # a single test fn
```

There is no JS test runner and no separate lint step — `npm run build` runs `tsc` (strict) as the typecheck gate. Rust is the source of nearly all logic; prefer `cargo test` when changing backend behavior.

The Vite dev server is fixed to port **5173** (`strictPort`); `tauri dev` expects it there.

### Releases

Releases are signed + notarized + auto-updating and are driven by skills, **not** raw build commands:

- macOS: the `release-macos` skill → `scripts/release-mac.sh` (needs `.env`, see `.env.example` + `docs/RELEASE.md`).
- Windows: the `release-windows` skill → `scripts/release-win.ps1`. Windows is always a **follower** that merges its `windows-x86_64` entry into the *same* GitHub release macOS already created (via `scripts/merge-manifest.mjs`) — never clobber the mac signatures or notes.

A version bump must stay in sync across **`package.json`, `package-lock.json`, `src-tauri/Cargo.toml`, and `src-tauri/tauri.conf.json`** — the release skills handle this.

### Signing key — there is NO password

Full release procedure (macOS leads, Windows follows) is in **[docs/RELEASE.md](docs/RELEASE.md)** — including a `scripts/release-win.ps1` + `/release-windows` skill that automates the Windows side and a manual `.nsis.zip` fallback. The non-obvious facts that bite releases:

The Tauri updater signing key (`~/.tauri/agent-status-updater.key`) is **not password-protected**. Its base64 payload decodes to an `rsign encrypted secret key` header, but that "encrypted" marker is just the minisign/`rsign` format label — the key was generated with an **empty password**. When signing, always pass an empty password:

```bash
# env-var form (for `tauri build`):
export TAURI_SIGNING_PRIVATE_KEY="$(cat ~/.tauri/agent-status-updater.key)"
export TAURI_PRIVATE_KEY_PASSWORD=""
npx tauri build

# explicit CLI form (for signing a single artifact):
npx tauri signer sign -f ~/.tauri/agent-status-updater.key -p "" <file>
```

Never ask for or set a signing password — there isn't one. If a future release hangs on a password prompt or fails with a decryption error using an empty password, the key has changed and this note is stale. The public half embedded in `tauri.conf.json` (`plugins.updater.pubkey`, key id `RWQd3bhFhWatl…`) must match the secret key; the `tauri-plugin-updater` verifies every downloaded update against it and signature checks **cannot be disabled**.

> Build-troubleshooting note: if `tauri build` is invoked with the env vars set but the `.nsis.zip` + `.nsis.zip.sig` updater artifacts still don't appear (only the `.exe`), the bundled signing step silently failed. The `.nsis.zip` is just a **stored** (uncompressed) zip of the NSIS `.exe` with `0o755` perms (see `tauri-bundler/.../updater_bundle.rs::create_zip`), so it can be built and signed manually as a fallback, then the `windows-x86_64` entry in `latest.json` updated to point at it.

## Architecture

**Thin React frontend that only renders; rich Rust backend that does all the work.** All real logic (log parsing, vendor APIs, encryption, throttling, plan ceilings) lives in Rust. The frontend invokes commands and renders whatever snapshot comes back.

```
src/ (React)                         src-tauri/src/ (Rust)
  hooks/useUsage ─── invoke ───────▶ commands/usage.rs ── collect()
  hooks/useTauriCommand              scanner/   logs → UsageSnapshot
  components/Meter,WeekChart,…       vendors/   claude·glm·anthropic·copilot·alibaba·kimi·grok
  renders snapshot  ◀── event ────── encryption/ settings/ state/ storage/
                  "usage-updated"    tray.rs    menubar icon + dropdown
                                       ▲ lib.rs: background timer loop
```

### The snapshot is the single source of truth

`commands::usage::collect()` (in [src-tauri/src/commands/usage.rs](src-tauri/src/commands/usage.rs)) is the heart of the app. Every refresh path funnels through it:

1. Scan local logs off the async runtime via `spawn_blocking` (`scanner::scan_default`) → a base `UsageSnapshot` with **estimated** Claude meters and the cross-provider Sessions list.
2. Optionally overwrite the Claude meters with **live** `/usage` data (`vendors::claude::fetch`).
3. Fetch live GLM / Anthropic / Copilot / Kimi / Grok vendor status (network, async).
4. Compute `Detection` (which provider tabs to show) and attach the `VendorReport`.
5. Cache the merged snapshot in `AppState` and return it.

`collect()` is invoked from three places and **must stay consistent across all of them**: the `get_usage` command (frontend load + manual refresh), the `reconnect_claude` command, and the background timer loop in [src-tauri/src/lib.rs](src-tauri/src/lib.rs). Each emits a `usage-updated` event the frontend listens for.

### Concurrency & freshness invariants (don't break these)

- **`CollectLock`** (a `tokio::sync::Mutex` in [state/mod.rs](src-tauri/src/state/mod.rs), managed separately from `AppState` because it's held across `.await`) serializes all `collect()` calls. On window open, `refresh_on_open` and the frontend's `get_usage` fire near-simultaneously; without serialization they race the rate-limited live endpoint and emit conflicting estimate-vs-live snapshots.
- **Live `/usage` throttle**: the live Claude endpoint rate-limits hard, so it's fetched at most once per `LIVE_CLAUDE_MIN_SECS` (120s) regardless of the faster log-scan refresh interval. Between fetches, `collect()` serves `live_claude_buckets` (the last *good* live reading). Never fall back to the local estimate mid-stream — the two are on different scales and the meters would visibly flip-flop. The live-data state machine has distinct UI states (`live` / `pending` / `needs_reauth` / `signed_out`) — read the big `if live_claude { … }` block before touching it.
- **Out-of-order snapshots**: `Meta.generatedMs` stamps every snapshot; the frontend (`useUsage`'s `applySnapshot`) drops any snapshot older than what's displayed. Multiple emitters race, so this guard is load-bearing.
- The background loop **only polls while the window is visible** — no network calls when the dropdown is hidden.

### Sessions come from several log formats, and they disagree

`scanner::scan_roots` takes a `ScanRoots` (one path per provider) and merges session rows from every CLI that keeps a local log — see the table in the `scanner` module doc. The formats are not equivalent, and the differences are load-bearing:

- **Claude** and **Kimi** record per-turn token usage, so their rows are exact. Kimi splits usage across `agents/*/wire.jsonl` (main loop + one file per subagent), each turn recorded exactly once, so summing across agents is correct and summing anything else double-counts.
- **Grok / xAI** records billed spend on `turn_completed` usage objects (`inputTokens` / `outputTokens`, optionally split by `modelUsage`). Bare `_meta.totalTokens` is the context-window size and must not be counted. A session without billed usage reports `—`. Week/5h totals read every session active in the last 7 days; the Sessions list is still capped at `MAX_PROVIDER_ROWS`. Honor `$GROK_HOME` (default `~/.grok`).
- **Copilot** writes totals only in `session.shutdown`. A running session has none — that's `—`, not zero. Resumed sessions emit several shutdowns: current builds restore cumulative counters (don't sum them), older ones reset to zero (don't take the last), so `scan_copilot` takes the **max**.
- **GLM** and **Alibaba** have no per-session local data at all and never will from a log scan; don't add speculative parsing for them.

Token/cost figures are rendered to strings **in Rust** (`tokensText` / `costText`) because "unavailable" is provider-specific — dollars for Claude, premium requests for Copilot, `—` for a flat-rate plan. The UI must not re-derive them from the numeric fields, which are zero when unknown.

Both new scanners are on the refresh path, so they read only the newest `MAX_PROVIDER_ROWS` sessions per provider and prefilter lines by substring before parsing JSON. Copilot event logs run to megabytes; keep the cheap path cheap.

### Estimated vs. live data

Read [docs/RELEASE.md](docs/RELEASE.md) and the README's data-source table, but the core distinction: Claude **token usage** is exact (parsed from `~/.claude/projects/**/*.jsonl`), but the **"% left" ceilings** are estimates derived from an editable plan tier (Pro / Max 5× / Max 20×) in `scanner` — there's no public subscription-quota API. Turning on "live Claude" reads the Claude Code OAuth token to show the same numbers `/usage` shows. Costs are derived from hardcoded per-model pricing in `scanner::price()`.

### Vendors

Each `vendors/*.rs` client does a thin network call + pure, unit-tested JSON parsing, and **degrades to an error string instead of panicking** — a bad key or unreachable endpoint must never crash the scan. They return a uniform `VendorStatus { configured, ok, error, primary, secondary, detail }`. GLM/z.ai and Anthropic need an API key; Copilot reads a locally-discovered editor/`gh` token by default but prefers an in-app token from the GitHub **device flow** (`copilot_device_start` / `_poll` / `_cancel`, state held in `AppState::pending_copilot_device`); Kimi reads the Kimi Code CLI's stored OAuth login (`~/.kimi-code/credentials/kimi-code.json`, honoring `KIMI_CODE_HOME`) and **renews it in place**: access tokens live only ~15 min and the CLI refreshes only while running, so `collect()` auto-refreshes an expired-by-clock login via the CLI's own public OAuth client (`kimi::refresh`, throttled by `KIMI_REFRESH_MIN_SECS`) and writes the rotated (single-use) refresh token back to the shared file — race-safe because the CLI re-reads that file before/after its own refresh. Grok does the same against `~/.grok/auth.json` (`GROK_HOME`, `GROK_REFRESH_MIN_SECS`) but **only refreshes a SpaceXAI (`auth.x.ai`) issuer** — never a foreign OIDC entry. SuperGrok billing has no % ceiling; an unrecognized body is an error, not a fake “Grok Build” plan. A login that can't be refreshed is surfaced as `auth_expired`.

### Secrets

API keys/tokens are encrypted at rest with **AES-256-GCM + Argon2id** ([encryption/mod.rs](src-tauri/src/encryption/mod.rs)), keyed off the machine UID — so a copied `settings.json` can't be decrypted elsewhere. **Plaintext keys never cross the IPC boundary**: settings are persisted as `Settings` (holds `EncryptedSecret`s), but the frontend only ever receives `SettingsView`, which exposes booleans like `glmKeySet` — never ciphertext or plaintext. Preserve this split when adding any secret.

### Conventions

- **Rust → JS serialization is `camelCase`** (`#[serde(rename_all = "camelCase")]` on every output struct). TS types in [src/types.ts](src/types.ts) must mirror the Rust structs in `scanner` and `vendors`; change them together.
- New backend functionality = a `#[tauri::command]` registered in `lib.rs`'s `invoke_handler!`, exposed to the UI through a typed `useTauriCommand<T>` wrapper composed inside `useUsage`. Don't call `invoke` inline in components.
- All file I/O in `scanner` is synchronous and **must** be called via `spawn_blocking` from async commands.
- Frontend code that may run before the IPC bridge is ready must guard with `isTauriReady()`.
- The same frontend bundle renders three contexts keyed off the window label: the `main` dropdown, the `hover` popover (compact preview, see `tray.rs`), and a `minimal` Overview (compact, window auto-fit). `App.tsx` branches on these.
- `panic = "abort"` is set, so a panic kills the whole app — untrusted JSON (e.g. OAuth `expires_in`) is clamped and arithmetic uses checked/`try_` variants.
- `open_url` is scheme-restricted to http(s) so it can't be abused as a generic process launcher; Windows uses `rundll32 url.dll,FileProtocolHandler` (not `cmd /C start`) to avoid shell metacharacter re-interpretation.
