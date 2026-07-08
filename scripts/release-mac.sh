#!/usr/bin/env bash
#
# Build, sign (Developer ID), and notarize the macOS .app + .dmg.
#
# Prerequisites:
#   - A "Developer ID Application" cert in your login keychain
#   - Notarization credentials (see .env.example / docs/RELEASE.md)
#
# Usage:
#   cp .env.example .env        # then fill in your credentials
#   ./scripts/release-mac.sh            # build + sign + notarize, write latest.json
#   ./scripts/release-mac.sh --publish  # also create the GitHub release + upload assets
#
set -euo pipefail

PUBLISH=false
for arg in "$@"; do
  case "$arg" in
    --publish) PUBLISH=true ;;
    *) echo "unknown arg: $arg (supported: --publish)" >&2; exit 2 ;;
  esac
done

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# Load local credentials if present (.env is gitignored).
if [[ -f .env ]]; then
  set -a
  # shellcheck disable=SC1091
  source .env
  set +a
fi

if [[ -z "${APPLE_SIGNING_IDENTITY:-}" ]]; then
  echo "error: APPLE_SIGNING_IDENTITY is not set. Copy .env.example to .env and fill it in." >&2
  echo "       List your identities with: security find-identity -v -p codesigning" >&2
  exit 1
fi

# Notarization needs EITHER Apple ID creds OR an API key. Warn (don't fail) if
# neither is present so you can still produce a signed-but-unnotarized build.
notarize=true
if [[ -z "${APPLE_API_KEY:-}" && ( -z "${APPLE_ID:-}" || -z "${APPLE_PASSWORD:-}" ) ]]; then
  notarize=false
  echo "warning: no notarization credentials found — building SIGNED but NOT notarized." >&2
  echo "         The DMG will trigger Gatekeeper warnings on other Macs." >&2
fi

# Universal (Intel + Apple Silicon) by default. Set TARGET=aarch64-apple-darwin
# (or x86_64-apple-darwin) to build host-native / single-arch instead.
TARGET="${TARGET:-universal-apple-darwin}"

echo "==> Signing identity: ${APPLE_SIGNING_IDENTITY}"
echo "==> Notarization: $([[ "$notarize" == true ]] && echo enabled || echo disabled)"
echo "==> Target: ${TARGET}"
echo

# Tauri signs with the hardened runtime when APPLE_SIGNING_IDENTITY is set,
# notarizes + staples automatically when notarization creds are present, and
# produces the updater artifacts (.app.tar.gz + .sig) when an updater signing
# key is set in the environment.
#
# The DMG bundler's final step runs a Finder AppleScript to style the disk-image
# window. That call needs an Automation grant to control Finder and a responsive
# Finder, and it hangs indefinitely on machines where a FinderSync extension
# (e.g. Synology Drive) wedges Finder when the temp DMG volume mounts. We have no
# custom DMG styling configured, so skip the cosmetics by default: Tauri passes
# --skip-jenkins to bundle_dmg.sh when CI is set. Run with DMG_STYLED=true to opt
# back into the Finder styling (only works at the machine with Automation granted
# and the FinderSync extension quit/disabled).
if [[ "${DMG_STYLED:-false}" != "true" ]]; then
  export CI=true
fi

npx tauri build --target "$TARGET" --bundles app,dmg

# Bundles live under target/<triple>/release when --target is passed.
APP="src-tauri/target/${TARGET}/release/bundle/macos/Agent Usage Monitor.app"
DMG_DIR="src-tauri/target/${TARGET}/release/bundle/dmg"

echo
echo "==> Verifying code signature"
codesign --verify --deep --strict --verbose=2 "$APP"

echo
echo "==> Gatekeeper assessment (must say: accepted / source=Notarized Developer ID)"
spctl --assess --type execute --verbose=2 "$APP" || {
  echo "spctl rejected the app — check signing/notarization above." >&2
}

if [[ "$notarize" == true ]]; then
  echo
  echo "==> Verifying notarization ticket is stapled"
  xcrun stapler validate "$APP" || echo "stapler validate failed — see docs/RELEASE.md" >&2
fi

# --- Merge our platform entry into the shared updater manifest ---------------
# agent-status ships from two machines (this Mac + a Windows PC) into ONE
# latest.json per release. We MERGE rather than overwrite, so the Windows
# build's signature (added later, against the same version) is never clobbered —
# and vice versa. scripts/merge-manifest.mjs is the single, version-aware merge
# point: same version keeps the other platform's entries, a new version starts
# fresh. The manifest is tracked in git so the Windows machine can pull the mac
# signatures and merge into the same version.
BUNDLE_DIR="src-tauri/target/${TARGET}/release/bundle"
MACOS_DIR="${BUNDLE_DIR}/macos"
TARBALL="${MACOS_DIR}/Agent Usage Monitor.app.tar.gz"
SIG_FILE="${TARBALL}.sig"
DMG_FILE=$(ls -1 "$DMG_DIR"/*.dmg 2>/dev/null | head -1 || true)

# Version is the source of truth in tauri.conf.json.
VERSION=$(grep -m1 '"version"' src-tauri/tauri.conf.json | sed -E 's/.*"version" *: *"([^"]+)".*/\1/')
REPO=$(gh repo view --json nameWithOwner -q .nameWithOwner 2>/dev/null || echo "dennisrongo/agent-status")

# Tracked, cross-machine source of truth (committed to git).
MANIFEST="updater/latest.json"

if [[ -f "$SIG_FILE" ]]; then
  # GitHub rewrites spaces in asset names to dots — match that in the URL.
  ASSET_NAME=$(basename "$TARBALL" | tr ' ' '.')
  URL="https://github.com/${REPO}/releases/download/v${VERSION}/${ASSET_NAME}"
  # The updater resolves by running arch (darwin-aarch64 / darwin-x86_64); it
  # does NOT match "darwin-universal". A universal payload satisfies both, so we
  # list both keys pointing at the same tarball + signature.
  node scripts/merge-manifest.mjs \
    --manifest "$MANIFEST" \
    --version "$VERSION" \
    --platforms "darwin-aarch64,darwin-x86_64" \
    --sig-file "$SIG_FILE" \
    --url "$URL"
  echo
  echo "==> Merged darwin entries into updater manifest: $MANIFEST"
  echo "    Commit + push this file so the Windows build merges into the same version."
else
  echo "warning: no updater .sig found — set TAURI_SIGNING_PRIVATE_KEY in .env to enable auto-updates." >&2
  MANIFEST=""
fi

echo
echo "==> Done. Artifacts:"
[[ -n "$DMG_FILE" ]] && ls -1 "$DMG_FILE"
ls -1d "$APP" 2>/dev/null || true
[[ -f "$SIG_FILE" ]] && ls -1 "$TARBALL" "$SIG_FILE"
[[ -n "$MANIFEST" ]] && ls -1 "$MANIFEST"

# --- Publish to GitHub Releases (opt-in) -------------------------------------
if [[ "$PUBLISH" == true ]]; then
  if [[ -z "$MANIFEST" ]]; then
    echo "error: refusing to publish without an updater manifest (no .sig)." >&2
    exit 1
  fi
  TAG="v${VERSION}"

  # Version-less convenience copy so the website can link to a stable URL
  # (…/releases/latest/download/Agent-Usage-Monitor-macOS.dmg) that always resolves
  # to the newest DMG without editing HTML each release. It's a byte-identical copy
  # of the signed+notarized DMG (staple travels with the bytes), used for human
  # downloads only — the auto-updater still uses the versioned .app.tar.gz.
  STABLE_DMG=""
  if [[ -n "$DMG_FILE" ]]; then
    STABLE_DMG="$(dirname "$DMG_FILE")/Agent-Usage-Monitor-macOS.dmg"
    cp -f "$DMG_FILE" "$STABLE_DMG"
  fi

  # Build release notes from the commit log since the previous release tag, so
  # the GitHub release records what actually changed in this version instead of
  # a static blurb. The Windows build only uploads assets (never edits the body),
  # so these notes survive intact across both machines.
  NOTES_FILE="$(mktemp -t agent-status-notes)"
  trap 'rm -f "$NOTES_FILE"' EXIT
  # Releases are tagged server-side by `gh release create`, so the local repo may
  # not have prior tags — fetch them before picking the previous one.
  git fetch --tags --quiet || true
  PREV_TAG=$(git tag -l 'v*' --sort=-v:refname | grep -v "^${TAG}$" | head -1 || true)
  {
    echo "## What's changed in ${TAG}"
    echo
    if [[ -n "$PREV_TAG" ]]; then
      CHANGES=$(git log "${PREV_TAG}..HEAD" --no-merges --pretty=format:'- %s (%h)' \
        | grep -viE 'bump version' || true)
      [[ -z "$CHANGES" ]] && CHANGES="- Maintenance and internal improvements."
      printf '%s\n\n\n' "$CHANGES"
      echo "**Full changelog:** https://github.com/${REPO}/compare/${PREV_TAG}...${TAG}"
    else
      echo "- Initial release."
    fi
    echo
    echo "---"
    echo
    echo "Download the installer for your platform below (\`.dmg\` for macOS, \`.exe\` for Windows)."
    echo "Installed apps auto-update via the in-app updater."
  } > "$NOTES_FILE"

  echo
  echo "==> Release notes for ${TAG} (from ${PREV_TAG:-<none>}..HEAD):"
  sed 's/^/    /' "$NOTES_FILE"
  echo
  if gh release view "$TAG" >/dev/null 2>&1; then
    echo "==> Release $TAG exists — refreshing notes + uploading/overwriting assets"
    gh release edit "$TAG" --notes-file "$NOTES_FILE"
    gh release upload "$TAG" --clobber \
      "$DMG_FILE" ${STABLE_DMG:+"$STABLE_DMG"} "$TARBALL" "$SIG_FILE" "$MANIFEST"
  else
    echo "==> Creating release $TAG on $REPO"
    gh release create "$TAG" \
      --title "$TAG — Agent Usage Monitor" \
      --notes-file "$NOTES_FILE" \
      "$DMG_FILE" ${STABLE_DMG:+"$STABLE_DMG"} "$TARBALL" "$SIG_FILE" "$MANIFEST"
  fi
  echo
  echo "==> Verifying the updater endpoint resolves unauthenticated"
  code=$(curl -sL -o /dev/null -w "%{http_code}" \
    "https://github.com/${REPO}/releases/latest/download/latest.json")
  echo "    latest.json -> HTTP $code $([[ "$code" == 200 ]] && echo '✓' || echo '(check repo is public)')"
else
  echo
  echo "Not published. Re-run with --publish to create/update the GitHub release,"
  echo "or upload the artifacts above to a release tagged v${VERSION} manually."
fi
