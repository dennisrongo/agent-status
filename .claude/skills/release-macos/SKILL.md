---
name: release-macos
description: Cut a signed + notarized + auto-updating macOS release of this Tauri app (agent-status) end to end — bump the version in every file, build a universal DMG, notarize, regenerate the per-arch updater manifest, and publish the GitHub release. Use this skill whenever the user says "cut a release", "release the app", "ship a new version", "publish a release", "make a new dmg", "bump and release", or "/release-macos" — even if they don't name the skill. Do not trigger for plain `cargo build` / `npm run tauri build` (local dev builds with no signing or publish).
---

# Release macOS

Orchestrates a full production release of **agent-status** (a Tauri 2 macOS menubar app): version bump → universal build → Developer ID signing → Apple notarization → updater manifest → GitHub release. Writes no production code — it drives the existing `scripts/release-mac.sh` and verifies the result.

## When to use this skill

- "cut a release", "cut a 0.1.2 release", "release the app", "ship a new version"
- "publish a release", "make a new dmg", "bump and release", "/release-macos"
- The user wants installed copies to receive an auto-update.

Do **not** trigger for a local dev build (`npm run tauri dev`, `cargo build`) — those don't sign, notarize, or publish.

## Workflow

1. **Determine the new version — automatically, from the commits since the last release.** If the user named an explicit version or bump level (e.g. "ship 0.2.0", "cut a minor"), honor it. Otherwise classify the Conventional-Commit subjects in the `PREV_TAG..HEAD` range (the same range the release notes use) and pick the bump. The rule is **version-aware** because the app is pre-1.0:
   - **minor** (`0.1.2 → 0.2.0`) — any `feat:` / `feat(scope):` commit, **or** any breaking change while still pre-1.0 (`major == 0`). Pre-1.0, a breaking change does **not** auto-jump to `1.0.0` — that's a deliberate call you make by asking explicitly.
   - **patch** (`0.1.2 → 0.1.3`) — only fixes/chores and no feature (`fix:`, `chore:`, `docs:`, `refactor:`, `perf:`, …).
   - **major** (`0.9.0 → 1.0.0`) — a breaking change (`!` before the colon, e.g. `feat!:`, or a `BREAKING CHANGE` footer) **only once already at `1.x`+**. Pre-1.0 these stay minor per the rule above.

   Compute and **announce** it (never silently bump) — print the choice and the commits that drove it:
   ```bash
   git fetch --tags --quiet
   PREV_TAG=$(git tag -l 'v*' --sort=-v:refname | head -1)
   RANGE="${PREV_TAG:+$PREV_TAG..}HEAD"
   CUR=$(node -p "require('./src-tauri/tauri.conf.json').version"); MAJOR=${CUR%%.*}
   breaking() { git log $RANGE --no-merges --pretty=format:'%s'  | grep -qE '^[a-z]+(\([^)]*\))?!:' \
             || git log $RANGE --no-merges --pretty=format:'%B' | grep -q  'BREAKING CHANGE'; }
   feature()  { git log $RANGE --no-merges --pretty=format:'%s'  | grep -qE '^feat(\([^)]*\))?:'; }
   if   [[ $MAJOR -ge 1 ]] && breaking; then BUMP=major          # only after 1.0.0
   elif feature || breaking;            then BUMP=minor          # pre-1.0: breaking caps at minor
   else                                      BUMP=patch
   fi
   echo "Recommended: $BUMP bump  (current $CUR; commits since ${PREV_TAG:-<none>})"
   git log $RANGE --no-merges --pretty=format:'  %s'
   ```
   State the resulting version and the `feat:`/breaking commit(s) behind a minor (or major), then proceed. The new version MUST be strictly greater than the current `tauri.conf.json` version — the auto-updater only fires on a newer one, so never re-publish an existing version. If there are no commits since the last tag, there is nothing to release — stop and say so.
2. **Bump the version in all five files, kept identical:**
   - `package.json` (`"version"`)
   - `package-lock.json` (both the top-level `"version"` and `packages.""` `"version"`)
   - `src-tauri/Cargo.toml` (`[package] version`)
   - `src-tauri/Cargo.lock` (the `name = "agent-status"` package entry)
   - `src-tauri/tauri.conf.json` (`"version"` — this is the value shown in-app and written into `latest.json`)
   Then confirm all five match (`grep`).
3. **Typecheck:** `npm run build`. Fix any error before continuing.
4. **Commit + push (signed):** stage, verify no secret is staged (`git diff --cached --name-only | grep -iE '\.env$|\.key$|\.p8$'` must be empty), commit the bump with a **signed** commit (`git commit -S`) so it shows the "Verified" badge on GitHub, then `git push origin main`. (One-time SSH-signing setup: see `docs/RELEASE.md` → *Verified commits*.)
5. **Build + publish:** `./scripts/release-mac.sh --publish`. This builds universal (Intel + ARM), signs with Developer ID, notarizes + staples, **merges** the `darwin-aarch64` + `darwin-x86_64` entries into the tracked `updater/latest.json` (via `scripts/merge-manifest.mjs`, so a later Windows build can add `windows-x86_64` without clobbering these), generates **release notes from the commit log** since the previous tag, and creates the GitHub release `vX.Y.Z` (or refreshes notes + re-uploads if it already exists). Requires `.env` (Apple creds + `TAURI_SIGNING_PRIVATE_KEY`); the first signing of a session may need keychain "Always Allow".
6. **Commit the manifest (signed):** the script wrote the mac signatures into `updater/latest.json`. Commit + push it so the Windows build merges into the same version:
   ```bash
   git add updater/latest.json
   git commit -S -m "vX.Y.Z: add darwin updater signatures"
   git push origin main
   ```
7. **Verify externally** that the public endpoint serves the new version:
   ```bash
   curl -sL https://github.com/dennisrongo/agent-status/releases/latest/download/latest.json \
     | python3 -c "import sys,json;d=json.load(sys.stdin);print(d['version'],list(d['platforms']))"
   curl -sL -o /dev/null -w "tar.gz %{http_code}\n" https://github.com/dennisrongo/agent-status/releases/download/v<VERSION>/Agent.Usage.Monitor.app.tar.gz
   ```
   Expect the new version, both `darwin-aarch64` + `darwin-x86_64` keys, and HTTP 200.
8. **Report:** release URL, notarization status, the generated release notes, endpoint check, and that installed builds will catch the update on next launch. If a Windows build is wanted for this version, it follows via **`/release-windows`** (merges `windows-x86_64` into the same release).

See `docs/RELEASE.md` for the full runbook and `scripts/release-mac.sh` for the build itself.

## Examples

### Example 1: Patch release

**User:** "ship a 0.1.2 release"

**Claude:**
- Bumps all five files to 0.1.2, `npm run build`, commit + push.
- Runs `./scripts/release-mac.sh --publish`, then curls the endpoint and confirms `0.1.2` with both arch keys at HTTP 200.
- Reports the release URL and that installed `0.1.x` will show the update banner on next launch.

### Example 2: Unspecified bump (auto-decided)

**User:** "cut a new release"

**Claude:** Inspects the commits since the last tag (`v0.4.0`). Finds a `feat(ui): add per-session history sparkline` (and even a `BREAKING CHANGE` footer would stay minor pre-1.0), so picks a **minor** bump and announces: *"0.5.0 — minor, driven by `feat(ui): add per-session history sparkline`"*, then proceeds through the workflow. Had the range held only `fix:`/`chore:` commits, it would pick a patch bump (`0.4.0 → 0.4.1`) instead — no question asked unless the user wants to override.

## Anti-patterns

- ❌ Writing `latest.json` with a `darwin-universal` key — the updater matches the **running arch** (`darwin-aarch64` / `darwin-x86_64`) and ignores `darwin-universal`. List both arch keys pointing at the one universal payload (the script already does this).
- ❌ Making or leaving the repo private — release assets 404 for the unauthenticated updater and for DMG downloads. It must stay public.
- ❌ Using `TAURI_SIGNING_PRIVATE_KEY_PATH` — the build reads `TAURI_SIGNING_PRIVATE_KEY` (a path or the key contents). The `_PATH` name is silently ignored and no `.sig` is produced.
- ❌ Committing `.env` or the updater private key, or echoing their contents.
- ❌ Re-publishing the same version (or a lower one) — installed apps won't update. Always bump first.
- ❌ Bumping only some of the five version files — a mismatch means a confusing in-app version, a manifest that doesn't match the binary, or a stale `package-lock.json`.
- ❌ Hand-writing `latest.json` from scratch — go through `scripts/merge-manifest.mjs` (the script does). A from-scratch darwin-only manifest would wipe a `windows-x86_64` entry a Windows build added for the same version.
- ❌ Forgetting to commit `updater/latest.json` — the Windows build pulls it to learn the mac signatures; a stale committed manifest makes Windows merge against the wrong version.
- ✅ Bump everywhere → typecheck → signed commit → `--publish` → signed commit of `updater/latest.json` → verify the live endpoint.

## Notes

- **Back up `~/.tauri/agent-status-updater.key`.** It signs the update payload; losing it means no existing install can ever auto-update again (they'd each need a manual reinstall). The Windows machine needs a copy of this same key.
- **macOS leads, Windows follows.** This skill owns the version bump and creates the release with its changelog notes. `updater/latest.json` is tracked in git as the cross-machine source of truth; the Windows build (`/release-windows`) merges `windows-x86_64` into the same version + release without touching the notes.
- Notarization is not cached — each release re-submits to Apple and waits for "Accepted".
- Run the script without `--publish` to do the full build + manifest locally without touching GitHub (useful for a dry run).
