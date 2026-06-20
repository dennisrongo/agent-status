---
name: release-windows
description: Cut the Windows side of an agent-status release — build the NSIS .exe installer, sign the auto-update payload with the shared Tauri updater key, and MERGE the windows-x86_64 entry into the SAME GitHub release the macOS build already created (without clobbering the mac signatures or the release notes). Use this skill whenever the user says "release windows", "build the windows exe", "cut the windows release", "ship the windows build", "do the windows side", or "/release-windows" — even if they don't name the skill. Windows is always a FOLLOWER of an existing macOS release; do not trigger for a local dev build (`npm run tauri dev`).
---

# Release Windows

Cuts the **Windows half** of an agent-status release. macOS is the leader (it owns the version bump and creates the GitHub release); Windows **follows** — it builds the `.exe`, signs the update payload with the *same* updater key, and splices `windows-x86_64` into the one `latest.json` the Mac published. It drives `scripts/release-win.ps1` and writes no production code.

Run this **on a Windows machine**, after the macOS release for the same version is already published.

## Mental model — Windows follows, never leads

- **One updater key, all platforms.** The app verifies every platform's update against the single `pubkey` in `src-tauri/tauri.conf.json`. So the Windows machine must sign with a **copy of the same** `~/.tauri/agent-status-updater.key` the Mac uses (`TAURI_SIGNING_PRIVATE_KEY`). The key is a secret — copy it over, never commit it.
- **One manifest, merged not overwritten.** There is one `latest.json` per release. The Mac committed `updater/latest.json` with the `darwin-*` signatures. Windows reads it and *adds* `windows-x86_64` via `scripts/merge-manifest.mjs` (same version → keeps darwin). Overwriting it would wipe the Mac's signatures off the live endpoint — the whole thing this flow prevents.
- **Same version, same release tag.** Windows does **not** bump the version and does **not** create a release. It reads the version from `tauri.conf.json` and uploads into the existing `vX.Y.Z` tag with `--clobber`, which never edits the release body — so the Mac's changelog notes stay intact.

## When to use this skill

- "release windows", "build the windows exe", "cut the windows release", "ship the windows build", "/release-windows"
- The macOS release for this version is already published and you now want Windows users to get the same update.

Do **not** trigger for a local dev build (`npm run tauri dev`, `cargo build`).

## Prerequisites (one-time per Windows machine)

- **Toolchain:** Node + npm, Rust (MSVC toolchain), and the Tauri prerequisites (WebView2 is present on Win10/11). `gh` authenticated (`gh auth status`), `git` configured.
- **Updater key:** copy `~/.tauri/agent-status-updater.key` from the Mac, then set `TAURI_SIGNING_PRIVATE_KEY` (path or base64 contents) and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` in `.env`. **It must be the same key as macOS** or the signature won't validate against the embedded pubkey. `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` **must match the key's password** — set it to `""` if the key has none. The script signs via `tauri signer sign -p <password>`, so an empty password works; a wrong/omitted one fails the preflight's *"Updater key can sign"* check in ~2s (before any build).
- **Verified commits (for the "Verified" badge):** set up SSH commit signing once — see `docs/RELEASE.md` → *Verified commits*. Then the manifest commit below shows as Verified on GitHub.

> Verify all of the above at once with `./scripts/release-win.ps1 -Preflight` (checklist only — builds nothing).

## Workflow

0. **Preflight — run this first, especially on a new machine.** `./scripts/release-win.ps1 -Preflight` prints a ✓/✗ readiness checklist with the exact fix for each gap, and builds/uploads nothing:
   - toolchain — Node/npm, Rust + **MSVC** toolchain, Tauri CLI
   - `gh` authenticated
   - updater key resolves (`TAURI_SIGNING_PRIVATE_KEY` → an existing file or inline key)
   - on `main`, up to date with the remote; the `tauri.conf.json` version
   - `updater/latest.json` is at that version **with the mac's `darwin-*` signatures**
   - the GitHub release `vX.Y.Z` already exists

   Resolve every `[FAIL]` before continuing. (A normal run below executes this same checklist automatically and aborts on any blocker, so you never start a long build on a machine that isn't ready.)
1. **Pull the macOS release commit.** `git pull origin main`. This brings the version bump **and** `updater/latest.json` carrying the Mac's `darwin-*` signatures for this version.
2. **Read (don't bump) the version.** The version is whatever `src-tauri/tauri.conf.json` says — macOS already set it. Never bump on the Windows side.
3. **Confirm the macOS release exists.** `gh release view v<VERSION>` must succeed. If it doesn't, stop — the macOS release must be cut first (Windows only follows). `scripts/release-win.ps1` also enforces this and that `updater/latest.json` is already at this version.
4. **Typecheck:** `npm install` (if deps changed) then `npm run build`. Fix any error before continuing.
5. **Build + publish:** `./scripts/release-win.ps1 -Publish`. This builds the NSIS installer (with build-time signing disabled), packs the **version-matched** installer into a **STORED (uncompressed) `.nsis.zip` via Windows bsdtar**, signs that zip with the `tauri signer` CLI, **merges** `windows-x86_64` into `updater/latest.json` (preserving the darwin entries), and uploads the `.exe` + `.nsis.zip` + `.sig` + `latest.json` into the existing `v<VERSION>` release with `--clobber`. The release notes are left untouched. (Both the STORED zip and the version-matched installer selection are load-bearing for a working auto-update — see *Notes*.)
6. **Commit the merged manifest:** verify no secret is staged (`git diff --cached --name-only` must not include `.env` or `*.key`), then commit `updater/latest.json` with a **signed** commit and push:
   ```bash
   git add updater/latest.json
   git commit -S -m "v<VERSION>: add windows-x86_64 updater signature"
   git push origin main
   ```
7. **Verify externally** that the public endpoint serves all three platforms **and that the Windows payload is a STORED zip at the right version** (this is the check that catches a broken auto-update before users hit it):
   ```bash
   # manifest: version + platforms, and the windows URL must point at THIS version's zip
   curl -sL https://github.com/dennisrongo/agent-status/releases/latest/download/latest.json \
     | python3 -c "import sys,json;d=json.load(sys.stdin);print(d['version'],sorted(d['platforms']));print(d['platforms']['windows-x86_64']['url'])"

   # download that windows zip and assert STORED (compress_type 0) + integrity
   curl -sL -o /tmp/win.zip "<the windows-x86_64 url printed above>"
   python3 -c "import zipfile;z=zipfile.ZipFile('/tmp/win.zip');print('methods',[i.compress_type for i in z.infolist()]);print('integrity','OK' if z.testzip() is None else 'BAD')"
   ```
   Expect: the released version, `['darwin-aarch64','darwin-x86_64','windows-x86_64']`, a windows URL containing `_<VERSION>_x64-setup.nsis.zip` (not an older version), **methods `[0]` (STORED — `8` means DEFLATE and the client update will fail)**, and integrity `OK`. A DEFLATE payload or a stale version in the URL is a release-breaking regression — re-run step 5 and re-check.
8. **Report:** release URL, that the manifest now has all three platform keys with the mac signatures intact, that the release notes were preserved, and that installed Windows builds will catch the update on next launch.

See `docs/RELEASE.md` → *Releasing the Windows app* for the full runbook and `scripts/release-win.ps1` for the build itself.

## Examples

### Example 1: Following a fresh macOS release

**User:** "I just shipped 0.3.0 on the Mac — now do the windows side"

**Claude:**
- On the Windows box: `git pull`, confirms `tauri.conf.json` is `0.3.0` and `gh release view v0.3.0` succeeds.
- Runs `./scripts/release-win.ps1 -Publish`; it merges `windows-x86_64` into `updater/latest.json` (darwin entries preserved) and uploads the `.exe`/`.nsis.zip`/`.sig`/`latest.json` into `v0.3.0`.
- Commits the manifest with a signed commit, curls the endpoint, confirms all three platform keys at HTTP 200, and reports that the Mac's signatures + release notes are intact.

### Example 2: macOS hasn't released yet

**User:** "cut the windows release for 0.4.0"

**Claude:** Finds `gh release view v0.4.0` fails (or `updater/latest.json` isn't at 0.4.0) and stops, explaining that macOS must release `v0.4.0` first — Windows only follows so it can merge into the Mac's manifest without dropping the darwin signatures.

## Anti-patterns

- ❌ Bumping the version on the Windows side — macOS owns the version. Windows reads it from `tauri.conf.json` and builds exactly that.
- ❌ Creating a new release or a different tag — always upload into the Mac's existing `vX.Y.Z`. A second release/tag splits the update channel.
- ❌ Overwriting `latest.json` from scratch (or skipping `merge-manifest.mjs`) — that drops the `darwin-*` signatures and breaks macOS auto-updates. Always **merge**.
- ❌ Editing the release notes / `--notes` on upload — leave the Mac's changelog. `gh release upload --clobber` (used by the script) never touches the body; don't run `gh release edit`.
- ❌ Using a different updater key than macOS — the signature won't validate against the embedded pubkey and no install will update. Copy the *same* key.
- ❌ Committing `.env` or the updater `.key`, or echoing their contents.
- ❌ Packing the `.nsis.zip` with PowerShell `Compress-Archive` — it produces a DEFLATE zip the client updater can't extract (*"Compression method not supported"*). The zip **must** be STORED; the script uses bsdtar for this.
- ❌ Trusting a bare `*_x64-setup.exe` glob — if a prior version's installer lingers in the bundle dir it sorts first (`0.2.1` before `0.3.0`) and you'd sign/ship the stale one. Always select the version-matched installer.
- ✅ Pull → confirm same version + existing release → `release-win.ps1 -Publish` (merges) → signed commit of `updater/latest.json` → verify the live endpoint shows all three platforms **and the windows zip is STORED at the right version**.

## Notes

- **SmartScreen:** the `.exe` is not Authenticode-signed (updater-signature-only by design), so Windows shows an "unknown publisher" warning on first install. The in-app auto-updater still works because it uses the Tauri updater signature, not Authenticode. Add a code-signing cert later if the warning matters.
- **Bundle output** lives under `src-tauri/target/release/bundle/nsis/` (host x64 build, no target triple). The updater key for NSIS is `windows-x86_64`.
- Run the script without `-Publish` to build + merge the manifest locally without touching GitHub (dry run).
- **Signing is done by `tauri signer sign`, not by `tauri build`.** The script disables `createUpdaterArtifacts` for the build, then zips the installer and signs the zip itself, passing the password with `-p`. This is deliberate: `tauri build`'s build-time signing prompts for the key password on the **console** when `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` is absent — and on Windows an env var set to `""` is *deleted*, so an empty password can never reach the build that way. The result was a silent hang at *"Decrypting updater signing key, expect a prompt for password."* If you ever see that message, the build is using build-time signing — the CLI-signing path here avoids it entirely.
- **The `.nsis.zip` MUST be STORED, not DEFLATE.** `tauri-plugin-updater`'s zip reader is built without the deflate feature, so a DEFLATE entry fails on the client with *"unsupported Zip archive: Compression method not supported"* and **no Windows install can update** — even though the manifest, signature, and HTTP 200 all look correct. tauri-bundler stores the installer uncompressed for exactly this reason. PowerShell `Compress-Archive` always emits DEFLATE (on PS 5.1 / .NET Framework even `-CompressionLevel NoCompression` and `System.IO.Compression.ZipArchive` still tag entries DEFLATE), so the script packs the zip with Windows' bundled **bsdtar** (`%SystemRoot%\System32\tar.exe -a -cf … --options zip:compression=store`). Verify with `python -c "import zipfile;print([i.compress_type for i in zipfile.ZipFile('x.nsis.zip').infolist()])"` → must be `[0]`.
- **Pick the version-matched installer, not the first glob hit.** A stale `*_x64-setup.exe` from a previous version can remain in `src-tauri/target/release/bundle/nsis/`; a bare glob sorts alphabetically and would sign/upload the older one (manifest says new version, payload is old). The script globs `*_<VERSION>_x64-setup.exe` and aborts if it's absent.
- **Keep `scripts/release-win.ps1` ASCII-only.** PowerShell 5.1 reads the script as ANSI, so a multi-byte char (e.g. an em dash `—`) inside a string mangles parsing and fails with *"Missing closing '}'"*. Use plain `-`/`--` in code; em dashes are fine only in this Markdown.
