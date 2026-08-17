#!/usr/bin/env bash
# tauri-doctor.sh - unstick the agent-status dev build on macOS.
#
# macOS does NOT get the Windows "Access is denied" class of errors: Unix lets
# you replace a running binary's file, so a stale app/sidecar process can't
# block the build. What DOES still happen:
#   1. A previous dev run (app + MCP sidecar) still alive and holding the
#      single-instance socket / tray, so the new run exits instantly.
#      -> default: kill repo processes.
#   2. Corrupted artifacts after an interrupted build (weird link errors).
#      -> --clean: cargo clean -p the app crates (deps stay cached).
#
# Usage:
#   ./scripts/tauri-doctor.sh              # kill app/sidecar processes, report
#   ./scripts/tauri-doctor.sh --clean      # cargo clean -p the app crates
#   ./scripts/tauri-doctor.sh --verify     # cargo check after fixing
#   ./scripts/tauri-doctor.sh --dry-run    # show what would be killed

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SRC_TAURI="$REPO_ROOT/src-tauri"
CLEAN=0; VERIFY=0; DRY=0
for arg in "$@"; do
    case "$arg" in
        --clean) CLEAN=1 ;;
        --verify) VERIFY=1 ;;
        --dry-run) DRY=1 ;;
        *) echo "unknown flag: $arg" >&2; exit 2 ;;
    esac
done

echo "tauri-doctor: repo = $REPO_ROOT"

# -- 1. App/sidecar processes --------------------------------------------------
echo
echo "[1] App/sidecar processes"
if ! command -v pgrep >/dev/null 2>&1; then
    echo "  pgrep not available on this platform - skipping (use tauri-doctor.ps1 on Windows)"
else
PIDS="$(pgrep -f 'agent-status(-mcp)?' | grep -v "^$$\$" || true)"
if [ -z "$PIDS" ]; then
    echo "  none found"
else
    for pid in $PIDS; do
        if [ "$DRY" -eq 1 ]; then
            echo "  [dry-run] would stop PID $pid ($(ps -p "$pid" -o comm=))"
        else
            echo "  stopping PID $pid ($(ps -p "$pid" -o comm=))"
            kill "$pid" 2>/dev/null || true
        fi
    done
fi
fi

# -- 2. Corrupted package artifacts --------------------------------------------
echo
if [ "$CLEAN" -eq 1 ]; then
    echo "[2] cargo clean -p agent-status -p agent-status-mcp"
    if [ "$DRY" -eq 1 ]; then
        echo "  [dry-run] would clean workspace packages"
    else
        (cd "$SRC_TAURI" && cargo clean -p agent-status -p agent-status-mcp)
    fi
else
    echo "[2] Skipped (pass --clean to cargo clean the app crates)"
fi

# -- 3. Verify ------------------------------------------------------------------
if [ "$VERIFY" -eq 1 ]; then
    echo
    echo "[3] cargo check --no-default-features"
    (cd "$SRC_TAURI" && cargo check --no-default-features) && echo && echo "  OK - build unblocked."
fi

echo
echo "Done. Re-run your dev command (npm run tauri dev)."
