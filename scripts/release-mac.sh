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

# --- Generate the updater manifest (latest.json) -----------------------------
BUNDLE_DIR="src-tauri/target/${TARGET}/release/bundle"
MACOS_DIR="${BUNDLE_DIR}/macos"
TARBALL="${MACOS_DIR}/Agent Usage Monitor.app.tar.gz"
SIG_FILE="${TARBALL}.sig"
DMG_FILE=$(ls -1 "$DMG_DIR"/*.dmg 2>/dev/null | head -1 || true)

# Version is the source of truth in tauri.conf.json.
VERSION=$(grep -m1 '"version"' src-tauri/tauri.conf.json | sed -E 's/.*"version" *: *"([^"]+)".*/\1/')
REPO=$(gh repo view --json nameWithOwner -q .nameWithOwner 2>/dev/null || echo "dennisrongo/agent-status")

if [[ -f "$SIG_FILE" ]]; then
  # GitHub rewrites spaces in asset names to dots — match that in the URL.
  ASSET_NAME=$(basename "$TARBALL" | tr ' ' '.')
  PUB_DATE=$(date -u +"%Y-%m-%dT%H:%M:%SZ")
  SIG=$(cat "$SIG_FILE")
  URL="https://github.com/${REPO}/releases/download/v${VERSION}/${ASSET_NAME}"
  MANIFEST="${BUNDLE_DIR}/latest.json"
  # The updater resolves by running arch (darwin-aarch64 / darwin-x86_64); it
  # does NOT match "darwin-universal". A universal payload satisfies both, so we
  # list both keys pointing at the same tarball + signature.
  cat > "$MANIFEST" <<JSON
{
  "version": "${VERSION}",
  "notes": "Agent Usage Monitor ${VERSION}",
  "pub_date": "${PUB_DATE}",
  "platforms": {
    "darwin-aarch64": {
      "signature": "${SIG}",
      "url": "${URL}"
    },
    "darwin-x86_64": {
      "signature": "${SIG}",
      "url": "${URL}"
    }
  }
}
JSON
  echo
  echo "==> Wrote updater manifest: $MANIFEST"
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
  echo
  if gh release view "$TAG" >/dev/null 2>&1; then
    echo "==> Release $TAG exists — uploading/overwriting assets"
    gh release upload "$TAG" --clobber \
      "$DMG_FILE" "$TARBALL" "$SIG_FILE" "$MANIFEST"
  else
    echo "==> Creating release $TAG on $REPO"
    gh release create "$TAG" \
      --title "$TAG — Agent Usage Monitor" \
      --notes "Signed & notarized universal build. Download the .dmg to install; the .app.tar.gz/.sig/latest.json drive the in-app auto-updater." \
      "$DMG_FILE" "$TARBALL" "$SIG_FILE" "$MANIFEST"
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
