# Releasing agent-status

**macOS leads, Windows follows.** The macOS flow (below) owns the version bump and
creates the GitHub release with its changelog notes; the
[Windows flow](#releasing-the-windows-app) then merges its installer into that
same release. Commits are [signed/Verified](#verified-commits) on both machines.

## Releasing the macOS app

How to produce a **signed + notarized** `Agent Usage Monitor.app` and `.dmg` that
installs cleanly on any Mac — no Gatekeeper "damaged / unidentified developer"
warnings.

> This app is distributed as a **direct download (DMG)**, signed with a
> **Developer ID Application** certificate and **notarized** by Apple. It is
> *not* sandboxed and is *not* intended for the Mac App Store (it reads
> `~/.claude` and `~/.zai` directly, which the App Store sandbox forbids).

---

## 1. One-time prerequisites

| Need | How |
| --- | --- |
| Apple Developer Program membership | https://developer.apple.com ($99/yr) |
| **Developer ID Application** certificate in your login keychain | Xcode → Settings → Accounts → Manage Certificates → **+** → *Developer ID Application*. Verify with `security find-identity -v -p codesigning` |
| Xcode command-line tools (`notarytool`, `stapler`) | `xcode-select --install` |
| Notarization credentials | App-specific password **or** an App Store Connect API key (see below) |

This machine already has the cert:

```
Developer ID Application: Lean Code Automation LLC (PMNAQBZ3KN)
```

### Notarization credentials — pick one

**Option A — Apple ID + app-specific password (simplest)**
1. Go to https://appleid.apple.com → *Sign-In & Security* → *App-Specific Passwords*.
2. Generate one (e.g. labelled `agent-status-notarize`).
3. Put it in `.env` as `APPLE_PASSWORD`, with `APPLE_ID` and `APPLE_TEAM_ID` (`PMNAQBZ3KN`).

**Option B — App Store Connect API key**
1. App Store Connect → *Users and Access* → *Integrations* → *App Store Connect API* → generate a key.
2. Download the `.p8` (one-time) and note the **Issuer ID** and **Key ID**.
3. Set `APPLE_API_ISSUER`, `APPLE_API_KEY`, `APPLE_API_KEY_PATH` in `.env`.

---

## 2. Configure credentials

```bash
cp .env.example .env
# edit .env — fill in your signing identity + notarization credentials
```

`.env` is gitignored; secrets never leave your machine.

---

## 3. Build

```bash
./scripts/release-mac.sh
```

The script loads `.env`, runs `tauri build`, then verifies the signature,
Gatekeeper assessment, and (when notarizing) the stapled ticket.

Under the hood Tauri:
- signs the `.app` with your Developer ID identity **and the hardened runtime**
  (required for notarization), applying `src-tauri/entitlements.plist`;
- submits the build to Apple's notary service and waits;
- **staples** the notarization ticket so the app validates offline.

> **First run:** macOS may prompt *"codesign wants to sign using key …"*. Click
> **Always Allow** so future builds don't block.

The script builds a **universal** binary by default (Intel + Apple Silicon).
Artifacts land under the target triple:

```
src-tauri/target/universal-apple-darwin/release/bundle/
  macos/Agent Usage Monitor.app
  dmg/Agent Usage Monitor_<version>_universal.dmg
  macos/Agent Usage Monitor.app.tar.gz       # updater payload
  macos/Agent Usage Monitor.app.tar.gz.sig   # updater signature
```

> Set `TARGET=aarch64-apple-darwin ./scripts/release-mac.sh` to build a single
> arch instead (faster; bundles then live under `target/aarch64-apple-darwin/…`).

---

## 4. Verify it's distributable

```bash
APP="src-tauri/target/universal-apple-darwin/release/bundle/macos/Agent Usage Monitor.app"

codesign --verify --deep --strict --verbose=2 "$APP"   # signature valid + sealed
spctl --assess --type execute --verbose=2 "$APP"       # want: source=Notarized Developer ID
xcrun stapler validate "$APP"                          # ticket stapled (works offline)
```

The real test: copy the `.dmg` to a *different* Mac (or one that has never seen
the app), open it, drag to Applications, launch. It should open with **no**
warning.

---

## 5. Publishing an auto-update

The app checks for updates on launch (`src/hooks/useUpdater.ts`) against:

```
https://github.com/dennisrongo/agent-status/releases/latest/download/latest.json
```

The update payload is signed with the **updater key** (separate from the Apple
cert) generated once via `npx tauri signer generate`. The public half lives in
`src-tauri/tauri.conf.json` → `plugins.updater.pubkey`; the private half is set
at build time via `TAURI_SIGNING_PRIVATE_KEY_PATH` in `.env`. **Back this key
up** — losing it means existing installs can never auto-update again.

To ship an update:

1. Bump `version` in **`package.json`**, **`package-lock.json`** (top-level +
   `packages.""`), **`src-tauri/Cargo.toml`**, **`src-tauri/Cargo.lock`**, and
   **`src-tauri/tauri.conf.json`** (keep all five in sync — `tauri.conf.json` is the
   value shown in the app and written into `latest.json`). Commit (signed — see
   [Verified commits](#verified-commits)).
2. Run **`./scripts/release-mac.sh --publish`**. This signs + notarizes, produces
   the updater payload, **merges** the `darwin-*` entries into the tracked
   `updater/latest.json` (via `scripts/merge-manifest.mjs`), **generates release
   notes from the commit log** since the previous tag, then creates the GitHub
   release `vX.Y.Z` (or refreshes notes + re-uploads if it exists) and verifies the
   public endpoint returns HTTP 200. It also uploads a version-less copy of the DMG
   named `Agent-Usage-Monitor-macOS.dmg`, so the website can link to the stable URL
   `…/releases/latest/download/Agent-Usage-Monitor-macOS.dmg` (a human-download
   convenience only — the updater still uses the versioned `.app.tar.gz`).
3. **Commit `updater/latest.json`** (it now holds the mac signatures) and push.
   This is the cross-machine source of truth — the Windows build pulls it to learn
   the mac signatures and merge `windows-x86_64` into the same version.

That's it — no hand-edited manifest. Running without `--publish` does everything
except touch GitHub, leaving the artifacts + `updater/latest.json` for you to
upload manually.

> **Why a tracked `updater/latest.json`?** There is one `latest.json` per release
> but two build machines (this Mac + a Windows PC). The merge helper is
> *version-aware*: building the **same** version keeps the other platform's
> signatures; a **new** version starts fresh (so a stale prior-version entry never
> points installs at an old payload). macOS leads; Windows follows — see
> [Releasing the Windows app](#releasing-the-windows-app).

The generated manifest looks like:

```json
{
  "version": "X.Y.Z",
  "notes": "Agent Usage Monitor X.Y.Z",
  "pub_date": "2026-06-17T00:00:00Z",
  "platforms": {
    "darwin-aarch64": {
      "signature": "<contents of the .app.tar.gz.sig>",
      "url": "https://github.com/dennisrongo/agent-status/releases/download/vX.Y.Z/Agent.Usage.Monitor.app.tar.gz"
    },
    "darwin-x86_64": {
      "signature": "<same .sig>",
      "url": "<same universal .app.tar.gz>"
    }
  }
}
```

> The updater matches the **running arch** (`darwin-aarch64` / `darwin-x86_64`),
> not `darwin-universal` — so list both keys. A universal payload satisfies both,
> so they share one signature + URL.

Installed apps poll `latest.json`, and when its `version` is newer than the
running build they show the in-app **"Update & restart"** banner. The current
version is shown at the bottom of the app's **Settings** tab.

> **Repo must stay public** for the updater endpoint and DMG links to resolve —
> private-repo release assets 404 for unauthenticated downloads.

### Single-arch builds

The release script builds **universal** by default. For a faster single-arch
build (e.g. CI smoke test):

```bash
TARGET=aarch64-apple-darwin ./scripts/release-mac.sh
```

---

## Releasing the Windows app

The Windows side **follows** an existing macOS release — it never bumps the
version or creates its own release. It builds the NSIS `.exe`, signs the
auto-update payload with the **same** Tauri updater key, and merges a
`windows-x86_64` entry into the one `latest.json` the Mac published. Driven by
`scripts/release-win.ps1`; orchestrated by the `/release-windows` skill.

### One-time prerequisites (Windows machine)

| Need | How |
| --- | --- |
| Node + npm, Rust (MSVC), Tauri prereqs | https://v2.tauri.app/start/prerequisites (WebView2 ships with Win10/11) |
| `gh` authenticated, `git` configured | `gh auth status` |
| **The same updater key as macOS** | Copy `~/.tauri/agent-status-updater.key` from the Mac, then set `TAURI_SIGNING_PRIVATE_KEY` (path or base64 contents) + `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` in `.env`. The app verifies every platform against the one pubkey in `tauri.conf.json`, so the key **must** match. |

> The `.exe` is **not** Authenticode-signed (updater-signature-only by design), so
> Windows SmartScreen warns "unknown publisher" on first install. The in-app
> auto-updater still works — it uses the Tauri updater signature, not Authenticode.

### Steps

0. **`./scripts/release-win.ps1 -Preflight`** — a readiness checklist (toolchain,
   `gh` auth, updater key, branch/pull state, manifest at this version with the mac
   signatures, the `vX` release) printed as ✓/✗ with the exact fix for each gap.
   Builds and uploads nothing. A normal run also runs this and aborts on any blocker.
1. **`git pull origin main`** — brings the version bump *and* `updater/latest.json`
   with the Mac's `darwin-*` signatures for this version.
2. Confirm `src-tauri/tauri.conf.json` is the version you expect, and that the
   macOS release exists: `gh release view v<VERSION>`. **Do not bump the version.**
3. `npm install` (if deps changed), then `npm run build` to typecheck.
4. **`./scripts/release-win.ps1 -Publish`** — builds NSIS, signs the payload,
   merges `windows-x86_64` into `updater/latest.json` (preserving darwin), and
   uploads the `.exe` + `.nsis.zip` + `.sig` + `latest.json` into the existing
   `v<VERSION>` release with `--clobber`. It also uploads a version-less copy of the
   installer named `Agent-Usage-Monitor-Windows-setup.exe` for the website's stable
   `…/releases/latest/download/Agent-Usage-Monitor-Windows-setup.exe` link (human
   download only; the updater uses the versioned `.nsis.zip`). The release notes are
   **left untouched**.
5. Commit + push `updater/latest.json` (signed commit — see below).
6. Verify the endpoint serves all three platforms:
   ```bash
   curl -sL https://github.com/dennisrongo/agent-status/releases/latest/download/latest.json \
     | python3 -c "import sys,json;d=json.load(sys.stdin);print(d['version'],sorted(d['platforms']))"
   ```
   Expect `['darwin-aarch64','darwin-x86_64','windows-x86_64']`.

The script **refuses** to run if `updater/latest.json` isn't already at this
version or the `v<VERSION>` release doesn't exist — that guard is what prevents a
Windows build from publishing a manifest that drops the Mac's signatures.

Bundle output (host x64 build, no target triple):

```
src-tauri/target/release/bundle/nsis/
  Agent Usage Monitor_<version>_x64-setup.exe           # installer
  Agent Usage Monitor_<version>_x64-setup.nsis.zip      # updater payload
  Agent Usage Monitor_<version>_x64-setup.nsis.zip.sig  # updater signature
```

---

## Verified commits

The "Verified" badge on GitHub commits comes from **signed commits** — separate
from the Apple cert, the Windows installer, and the Tauri updater signature. Set
up SSH commit signing once **per machine** (Mac and Windows):

```bash
# Use an SSH key you already have (or generate one) and tell git to sign with it:
git config --global gpg.format ssh
git config --global user.signingkey ~/.ssh/id_ed25519.pub
git config --global commit.gpgsign true
git config --global tag.gpgSign true
```

Then add that **public** key to GitHub as a **Signing key** (Settings → SSH and
GPG keys → New SSH key → key type **Signing Key**). This is separate from an
authentication key — add it with type *Signing Key* even if the same key is
already there for auth.

With `commit.gpgsign true`, every `git commit` is signed automatically and shows
**Verified** on GitHub. The release scripts don't commit — the `/release-macos`
and `/release-windows` skills do, using `git commit -S`. The release **tag** is
created server-side by `gh release create`, so GitHub marks it Verified with its
own web-flow key automatically.

---

## Troubleshooting

| Symptom | Fix |
| --- | --- |
| `spctl` says *rejected / Unnotarized Developer ID* | Notarization didn't run or failed — check `.env` creds and re-run. |
| Notary log shows entitlement/hardened-runtime errors | Tauri enables the hardened runtime automatically when signing; ensure you're signing (identity set), not just building. |
| `errSecInternalComponent` during signing | Keychain locked — `security unlock-keychain login.keychain`, or approve the GUI prompt. |
| Notarization stuck `In Progress` | Apple's queue; `notarytool` polls until done. Check `xcrun notarytool history` with your creds. |
| App opens but quits immediately on another Mac | Almost always missing notarization/staple, or an entitlement the binary actually needs — read the notary log: `xcrun notarytool log <submission-id>`. |
| Build fails: *signing private key not set* | `createUpdaterArtifacts` is on, so a build needs `TAURI_SIGNING_PRIVATE_KEY_PATH` in `.env` (the updater key, not the Apple cert). |
| Update banner never appears | `latest.json` `version` must be **newer** than the installed app, and its `signature` must be the exact contents of the matching `.sig`. |
