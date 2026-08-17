# tauri-doctor.ps1 - unstick the agent-status dev build on Windows.
#
# Fixes the two recurring failure modes:
#   1. Locked exe / "Access is denied" (Os code 5) from the Tauri build script:
#      running app or MCP sidecar processes hold target\debug\*.exe or
#      src-tauri\binaries\*.exe, so the build can't copy/link. -> -KillLockers
#      (default).
#   2. App exits instantly (0xffffffff) after a good compile: orphaned
#      msedgewebview2 children of a crashed run hold the app's EBWebView
#      user-data lock. -> -WebView.
#   3. Cargo lock/cache errors: "Blocking waiting for file lock on build
#      directory" from a dead cargo/rustc, or a corrupted artifact after an
#      interrupted build. -> -KillCargo and/or -CleanPackages.
#
# Usage:
#   ./scripts/tauri-doctor.ps1                  # kill lockers, report
#   ./scripts/tauri-doctor.ps1 -WebView         # also stop orphaned app WebViews
#   ./scripts/tauri-doctor.ps1 -KillCargo       # also stop orphaned cargo/rustc
#   ./scripts/tauri-doctor.ps1 -CleanPackages   # cargo clean -p the app crates
#   ./scripts/tauri-doctor.ps1 -Verify          # cargo check after fixing
#   ./scripts/tauri-doctor.ps1 -DryRun          # show what would be killed
#
# Safe to run any time: it only touches processes whose path is inside this
# repo's src-tauri tree or whose image is one of this repo's exe names, and
# (with -KillCargo) cargo/rustc processes, which respawn on the next build.

[CmdletBinding()]
param(
    [switch]$KillCargo,
    [switch]$CleanPackages,
    [switch]$WebView,
    [switch]$Verify,
    [switch]$DryRun
)

$ErrorActionPreference = 'Stop'
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$srcTauri = Join-Path $repoRoot 'src-tauri'
$targetDir = Join-Path $srcTauri 'target'
# Exe basenames this repo produces (app + MCP sidecar + the renamed sidecar in binaries\).
$exeNames = @('agent-status.exe', 'agent-status-mcp.exe', 'agent-status-mcp-x86_64-pc-windows-msvc.exe')

function Stop-RepoProcesses {
    param([string]$Reason, [scriptblock]$Filter)
    $killed = @()
    foreach ($proc in Get-Process) {
        $path = $null
        try { $path = $proc.Path } catch { continue }   # access denied on system procs
        if (-not $path) { continue }
        if (& $Filter $proc $path) {
            $killed += $proc
            if ($DryRun) {
                Write-Host "  [dry-run] would stop $($proc.Name) (PID $($proc.Id))  [$Reason]" -ForegroundColor Yellow
            } else {
                Write-Host "  stopping $($proc.Name) (PID $($proc.Id))  [$Reason]" -ForegroundColor Yellow
                Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
            }
        }
    }
    return $killed
}

Write-Host "tauri-doctor: repo = $repoRoot" -ForegroundColor Cyan

# -- 1. Locked exe / PermissionDenied -----------------------------------------
# Anything running out of src-tauri\target or src-tauri\binaries, and any
# process whose image is one of our exe names (wherever it was launched from),
# holds a Windows file lock the build script trips over.
Write-Host "`n[1] App/sidecar processes locking build outputs" -ForegroundColor Cyan
$lockers = Stop-RepoProcesses -Reason 'locks target/binaries' -Filter {
    param($proc, $path)
    ($path.StartsWith($targetDir, [StringComparison]::OrdinalIgnoreCase)) -or
    ($path.StartsWith((Join-Path $srcTauri 'binaries'), [StringComparison]::OrdinalIgnoreCase)) -or
    ($exeNames -contains (Split-Path $path -Leaf))
}
if (-not $lockers) { Write-Host '  none found' -ForegroundColor Green }

# -- 2. Orphaned app WebView2 processes ---------------------------------------
# When the app is force-killed, its msedgewebview2 children can survive and
# keep the EBWebView user-data dir locked; the next launch then dies instantly
# (exit 0xffffffff). Match by command line containing this app's identifier
# (tauri.conf.json) so Edge/Teams/VS Code WebViews are never touched.
if ($WebView) {
    Write-Host "`n[2] Orphaned app WebView2 processes (-WebView)" -ForegroundColor Cyan
    $appId = 'com.dennisrongo.agentstatus'
    $wv = @(Get-CimInstance Win32_Process -Filter "Name = 'msedgewebview2.exe'" |
        Where-Object { $_.CommandLine -and $_.CommandLine.Contains($appId) })
    if (-not $wv) {
        Write-Host '  none found' -ForegroundColor Green
    } else {
        foreach ($p in $wv) {
            if ($DryRun) {
                Write-Host "  [dry-run] would stop msedgewebview2 (PID $($p.ProcessId))  [app WebView orphan]" -ForegroundColor Yellow
            } else {
                Write-Host "  stopping msedgewebview2 (PID $($p.ProcessId))  [app WebView orphan]" -ForegroundColor Yellow
                Stop-Process -Id $p.ProcessId -Force -ErrorAction SilentlyContinue
            }
        }
    }
} else {
    Write-Host "`n[2] Skipped (pass -WebView to stop orphaned app WebViews)" -ForegroundColor DarkGray
}

# -- 3. Orphaned cargo/rustc holding the target dir lock ----------------------
# "Blocking waiting for file lock on build directory" with no visible build
# means a dead cargo/rustc kept the lock. Only kills cargo/rustc/rust-analyzer
# processes whose current directory is this repo - unless -KillCargo is given,
# in which case every cargo/rustc on the machine is stopped (they respawn).
if ($KillCargo) {
    Write-Host "`n[3] Orphaned cargo/rustc processes (-KillCargo)" -ForegroundColor Cyan
    $cargo = Stop-RepoProcesses -Reason 'orphaned cargo/rustc' -Filter {
        param($proc, $path)
        @('cargo', 'rustc', 'cargo-clippy', 'rust-analyzer') -contains $proc.Name
    }
    if (-not $cargo) { Write-Host '  none found' -ForegroundColor Green }
} else {
    Write-Host "`n[3] Skipped (pass -KillCargo to stop orphaned cargo/rustc)" -ForegroundColor DarkGray
}

# -- 4. Corrupted package artifacts -------------------------------------------
# Interrupted builds occasionally leave a half-written rlib/exe that fails the
# next link with cryptic errors. Clean just this workspace's packages - much
# faster than a full `cargo clean` (deps stay cached).
if ($CleanPackages) {
    Write-Host "`n[4] cargo clean -p agent-status -p agent-status-mcp" -ForegroundColor Cyan
    if ($DryRun) {
        Write-Host '  [dry-run] would clean workspace packages' -ForegroundColor Yellow
    } else {
        Push-Location $srcTauri
        try { cargo clean -p agent-status -p agent-status-mcp } finally { Pop-Location }
    }
} else {
    Write-Host "`n[4] Skipped (pass -CleanPackages to cargo clean the app crates)" -ForegroundColor DarkGray
}

# -- 5. Verify the build script + typecheck pass ------------------------------
if ($Verify) {
    Write-Host "`n[5] cargo check --no-default-features" -ForegroundColor Cyan
    Push-Location $srcTauri
    try {
        cargo check --no-default-features
        if ($LASTEXITCODE -eq 0) {
            Write-Host "`n  OK - build unblocked." -ForegroundColor Green
        } else {
            Write-Host "`n  cargo check still failing (exit $LASTEXITCODE) - see output above." -ForegroundColor Red
            exit $LASTEXITCODE
        }
    } finally { Pop-Location }
}

Write-Host "`nDone. Re-run your dev command (npm run tauri dev)." -ForegroundColor Cyan
if ($lockers -and -not $DryRun) {
    Write-Host 'Note: killed sidecar processes are usually respawned by MCP clients' -ForegroundColor DarkYellow
    Write-Host '(python/node MCP servers). If locks keep returning, stop the MCP client' -ForegroundColor DarkYellow
    Write-Host 'or disable the MCP export in the app settings while developing.' -ForegroundColor DarkYellow
}
