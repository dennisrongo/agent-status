# Releasing the macOS app

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

1. Bump `version` in **`package.json`**, **`src-tauri/Cargo.toml`**, and
   **`src-tauri/tauri.conf.json`** (keep all three in sync — `tauri.conf.json` is
   the value shown in the app and written into `latest.json`). Commit.
2. Run **`./scripts/release-mac.sh --publish`**. This signs + notarizes, produces
   the updater payload, **generates `latest.json` automatically** from the new
   `.sig`, then creates the GitHub release `vX.Y.Z` (or uploads to it if it already
   exists) and verifies the public endpoint returns HTTP 200.

That's it — no hand-edited manifest. Running without `--publish` does everything
except touch GitHub, leaving the artifacts + `latest.json` under
`src-tauri/target/<triple>/release/bundle/` for you to upload manually.

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
