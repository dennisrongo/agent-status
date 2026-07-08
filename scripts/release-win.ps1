<#
.SYNOPSIS
  Build the Windows installer (NSIS), sign the auto-update payload, and merge the
  Windows entry into the SAME release the macOS build already created.

.DESCRIPTION
  Windows is a FOLLOWER of the macOS release, never the leader:

    * It does NOT bump the version. macOS owns the version bump; this script reads
      the version straight out of src-tauri/tauri.conf.json and builds exactly that.
    * It uploads into the SAME GitHub release tag (vX.Y.Z) the Mac created. If that
      release does not exist yet, this script refuses — it never creates a divergent
      release.
    * It MERGES windows-x86_64 into updater/latest.json (preserving the Mac's
      darwin signatures via scripts/merge-manifest.mjs). It will not clobber them.
    * It only uploads assets (gh release upload --clobber), which never edits the
      release body — so the changelog notes the Mac wrote stay intact.

  The auto-update payload is signed with the SAME Tauri updater key as macOS
  (TAURI_SIGNING_PRIVATE_KEY). The app verifies every platform against the one
  pubkey in tauri.conf.json, so the Windows machine must have a COPY of that key.
  The key is a secret — copy it over (or paste its base64 contents), never commit it.

  Signing is done by the `tauri signer` CLI (not by `tauri build`), passing the
  password via -p. Build-time signing is deliberately disabled: it reads the
  password from $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD and, when that is unset,
  blocks on an interactive console prompt that hangs headless runs — and on
  Windows an empty password can't be passed via the environment at all (setting
  an env var to "" deletes it). Set TAURI_SIGNING_PRIVATE_KEY_PASSWORD in .env to
  the key's password ("" if it has none).

  This does NOT Authenticode-sign the .exe, so Windows SmartScreen will warn
  "unknown publisher" on first install. The auto-updater works regardless.

.PARAMETER Preflight
  Only run the readiness checklist (toolchain, gh auth, updater key, branch state,
  manifest, release) and exit. Nothing is built or uploaded. Use this first on a
  new machine to see exactly what's missing and how to fix each item.

.PARAMETER Publish
  Also create/Update the GitHub release assets. Without it, this builds + merges
  the manifest locally for inspection but touches nothing remote. A normal run
  always runs the preflight first and aborts on any [FAIL].

.EXAMPLE
  .\scripts\release-win.ps1 -Preflight   # readiness checklist only
  .\scripts\release-win.ps1              # dry run: build + merge manifest locally
  .\scripts\release-win.ps1 -Publish     # upload into the Mac's release
#>
[CmdletBinding()]
param(
  [switch]$Publish,
  [switch]$Preflight
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Die($msg) { Write-Error $msg; exit 1 }
function Assert-LastExit($what) {
  if ($LASTEXITCODE -ne 0) { Die "$what failed (exit $LASTEXITCODE)." }
}
function Has-Command($name) { return [bool](Get-Command $name -ErrorAction SilentlyContinue) }

# Sign a file with the Tauri updater key via the signer CLI, passing the password
# explicitly with -p so an empty password signs non-interactively (no console
# prompt). $Key is a file path or inline base64 contents. Two Windows quirks are
# handled here:
#   * The CLI auto-reads the key from TAURI_SIGNING_PRIVATE_KEY (--private-key) /
#     TAURI_SIGNING_PRIVATE_KEY_PATH (--private-key-path). Our explicit -f/-k flag
#     would collide ("cannot be used with"), so we clear both for the call.
#   * Windows PowerShell 5.1 silently DROPS empty-string args to native commands,
#     so `-p ""` would vanish (the file then gets eaten as the password). For an
#     empty password we run the signer from a temp .cmd, where "" survives.
# Returns the CLI output; the caller checks $LASTEXITCODE.
function Invoke-SignerSign {
  param([string]$Key, [string]$Password, [string]$File)
  $keyArgs = if (Test-Path -LiteralPath $Key -ErrorAction SilentlyContinue) { @('-f', $Key) } else { @('-k', $Key) }
  $savedK = [Environment]::GetEnvironmentVariable('TAURI_SIGNING_PRIVATE_KEY', 'Process')
  $savedP = [Environment]::GetEnvironmentVariable('TAURI_SIGNING_PRIVATE_KEY_PATH', 'Process')
  [Environment]::SetEnvironmentVariable('TAURI_SIGNING_PRIVATE_KEY', $null, 'Process')
  [Environment]::SetEnvironmentVariable('TAURI_SIGNING_PRIVATE_KEY_PATH', $null, 'Process')
  try {
    if ([string]::IsNullOrEmpty($Password)) {
      $bat = Join-Path ([System.IO.Path]::GetTempPath()) ("relwin-sign-{0}.cmd" -f $PID)
      $line = 'npx --no-install tauri signer sign {0} "{1}" -p "" "{2}"' -f $keyArgs[0], $keyArgs[1], $File
      Set-Content -LiteralPath $bat -Value @('@echo off', $line) -Encoding ascii
      try { return (& cmd /c $bat 2>&1) } finally { Remove-Item -LiteralPath $bat -ErrorAction SilentlyContinue }
    } else {
      return (& npx --no-install tauri signer sign @keyArgs -p $Password "$File" 2>&1)
    }
  } finally {
    [Environment]::SetEnvironmentVariable('TAURI_SIGNING_PRIVATE_KEY', $savedK, 'Process')
    [Environment]::SetEnvironmentVariable('TAURI_SIGNING_PRIVATE_KEY_PATH', $savedP, 'Process')
  }
}

# --- Preflight: a readiness checklist with an exact fix for each gap ----------
$script:Checks = @()
function Add-Check {
  param(
    [string]$Name,
    [ValidateSet('OK', 'WARN', 'FAIL')] [string]$Status,
    [string]$Detail = '',
    [string]$Fix = ''
  )
  $script:Checks += [pscustomobject]@{ Name = $Name; Status = $Status; Detail = $Detail; Fix = $Fix }
}

function Invoke-Preflight {
  param([bool]$ForPublish)
  $script:Checks = @()
  # Items needed to PUBLISH are FAIL only when publishing; WARN on a dry run.
  $pubSev = if ($ForPublish) { 'FAIL' } else { 'WARN' }

  # --- Toolchain ---
  if (Has-Command 'node') { Add-Check 'Node.js' 'OK' (node --version) }
  else { Add-Check 'Node.js' 'FAIL' '' 'Install Node LTS: winget install OpenJS.NodeJS.LTS (or https://nodejs.org).' }

  if (Has-Command 'npm') { Add-Check 'npm' 'OK' (npm --version) }
  else { Add-Check 'npm' 'FAIL' '' 'Ships with Node.js — install Node first.' }

  if (Has-Command 'cargo') { Add-Check 'Rust (cargo)' 'OK' (cargo --version) }
  else { Add-Check 'Rust (cargo)' 'FAIL' '' 'Install Rust: https://rustup.rs (rustup-init.exe).' }

  if (Has-Command 'rustc') {
    $hostLine = (rustc -vV | Select-String '^host:')
    $hostTriple = if ($hostLine) { ($hostLine.ToString() -replace '^host:\s*', '') } else { '' }
    if ($hostTriple -match 'pc-windows-msvc') {
      Add-Check 'Rust MSVC toolchain' 'OK' $hostTriple
    } else {
      Add-Check 'Rust MSVC toolchain' 'FAIL' $hostTriple 'Tauri needs MSVC: install VS Build Tools ("Desktop development with C++"), then rustup default stable-x86_64-pc-windows-msvc.'
    }
  } else {
    Add-Check 'Rust MSVC toolchain' 'FAIL' '' 'Install Rust first (rustup).'
  }

  if (Has-Command 'npx') {
    npx --no-install tauri --version *> $null
    if ($LASTEXITCODE -eq 0) { Add-Check 'Tauri CLI' 'OK' 'npx tauri available' }
    else { Add-Check 'Tauri CLI' 'FAIL' 'not installed locally' 'Run: npm install (provides the @tauri-apps/cli devDependency).' }
  } else {
    Add-Check 'Tauri CLI' 'FAIL' '' 'npx not found — install Node.js.'
  }

  # --- Updater signing key (same key as macOS) ---
  $key = $env:TAURI_SIGNING_PRIVATE_KEY
  if ([string]::IsNullOrWhiteSpace($key)) {
    Add-Check 'Updater key' 'FAIL' 'TAURI_SIGNING_PRIVATE_KEY not set' 'Copy ~/.tauri/agent-status-updater.key from the Mac, then set TAURI_SIGNING_PRIVATE_KEY in .env (path or base64 contents).'
  } elseif (Test-Path -LiteralPath $key -ErrorAction SilentlyContinue) {
    Add-Check 'Updater key' 'OK' "file: $key"
  } elseif ($key.Length -gt 100) {
    Add-Check 'Updater key' 'OK' 'inline base64 contents'
  } else {
    Add-Check 'Updater key' 'FAIL' "not a file, too short to be an inline key: $key" 'Point TAURI_SIGNING_PRIVATE_KEY at the key file copied from the Mac, or its base64 contents.'
  }

  # --- Updater key can actually sign (catches wrong/missing password) ---
  # Signing happens via `tauri signer sign -p <pw>` (build-time signing is
  # disabled because it would hang on a console password prompt). Prove the
  # key + TAURI_SIGNING_PRIVATE_KEY_PASSWORD combination signs non-interactively
  # so we fail in ~2s here instead of after a multi-minute build.
  if (-not [string]::IsNullOrWhiteSpace($key) -and (Has-Command 'npx')) {
    $pw = $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD; if ($null -eq $pw) { $pw = '' }
    $probe = Join-Path ([System.IO.Path]::GetTempPath()) ("relwin-signprobe-{0}.txt" -f $PID)
    Set-Content -LiteralPath $probe -Value 'probe' -Encoding ascii
    try {
      $out = Invoke-SignerSign -Key $key -Password $pw -File $probe
      if ($LASTEXITCODE -eq 0) {
        Add-Check 'Updater key can sign' 'OK' 'signs non-interactively'
      } else {
        $hint = (($out | Out-String).Trim() -split "`n" | Select-Object -First 1)
        Add-Check 'Updater key can sign' 'FAIL' $hint 'Set TAURI_SIGNING_PRIVATE_KEY_PASSWORD in .env to the key''s password (use "" if the key has none).'
      }
    } finally {
      Remove-Item -LiteralPath $probe -ErrorAction SilentlyContinue
      Remove-Item -LiteralPath "$probe.sig" -ErrorAction SilentlyContinue
    }
  }

  # --- Git / branch / freshness ---
  if (-not (Has-Command 'git')) {
    Add-Check 'Git' 'FAIL' '' 'Install Git for Windows: https://git-scm.com.'
  } else {
    git rev-parse --is-inside-work-tree *> $null
    if ($LASTEXITCODE -ne 0) {
      Add-Check 'Git repo' 'FAIL' '' 'Run this from inside the agent-status clone.'
    } else {
      $branch = (git rev-parse --abbrev-ref HEAD).Trim()
      if ($branch -eq 'main') { Add-Check 'On main branch' 'OK' $branch }
      else { Add-Check 'On main branch' 'WARN' $branch 'Releases publish from main; git checkout main unless intentional.' }

      git fetch --quiet 2>$null
      $behind = (git rev-list 'HEAD..@{u}' --count 2>$null)
      if ($LASTEXITCODE -eq 0 -and $behind -and ([int]$behind) -gt 0) {
        Add-Check 'Up to date with remote' 'WARN' "$behind commit(s) behind" 'Run: git pull  (gets the macOS version bump + signatures).'
      } else {
        Add-Check 'Up to date with remote' 'OK' ''
      }
    }
  }

  # --- GitHub CLI auth ---
  if (Has-Command 'gh') {
    gh auth status *> $null
    if ($LASTEXITCODE -eq 0) { Add-Check 'GitHub CLI auth' 'OK' '' }
    else { Add-Check 'GitHub CLI auth' $pubSev 'not authenticated' 'Run: gh auth login.' }
  } else {
    Add-Check 'GitHub CLI' $pubSev '' 'Install gh: winget install GitHub.cli (or https://cli.github.com).'
  }

  # --- Version / manifest / release (the "Windows follows macOS" guarantees) ---
  $version = $null
  try { $version = (Get-Content 'src-tauri/tauri.conf.json' -Raw | ConvertFrom-Json).version } catch {}
  if ($version) { Add-Check 'App version (tauri.conf.json)' 'OK' "v$version" }
  else { Add-Check 'App version (tauri.conf.json)' 'FAIL' '' 'Could not read version from src-tauri/tauri.conf.json.' }

  if (-not (Test-Path 'updater/latest.json')) {
    Add-Check 'Updater manifest present' 'FAIL' 'updater/latest.json missing' 'Run git pull — the macOS release commits it.'
  } else {
    try {
      $m = Get-Content 'updater/latest.json' -Raw | ConvertFrom-Json
      $plats = @($m.platforms.PSObject.Properties.Name)
      $darwin = @($plats | Where-Object { $_ -like 'darwin-*' })
      if ($version -and $m.version -ne $version) {
        Add-Check 'Manifest matches app version' 'FAIL' "manifest v$($m.version), app v$version" 'git pull, and ensure the macOS release for this version ran first (Windows follows).'
      } elseif ($darwin.Count -eq 0) {
        Add-Check 'Manifest has darwin signatures' 'WARN' 'no darwin-* entry yet' 'Normally macOS releases first. Expected only if Windows truly goes first.'
      } else {
        Add-Check 'Manifest matches app version' 'OK' "v$($m.version): $(($plats | Sort-Object) -join ', ')"
      }
    } catch {
      Add-Check 'Updater manifest readable' 'FAIL' '' 'updater/latest.json is unparseable.'
    }
  }

  if ($version -and (Has-Command 'gh')) {
    gh release view "v$version" *> $null
    if ($LASTEXITCODE -eq 0) { Add-Check 'GitHub release exists' 'OK' "v$version" }
    else { Add-Check 'GitHub release exists' $pubSev "v$version not found" 'Run the macOS release for this version first; Windows only uploads into it.' }
  }

  return $script:Checks
}

function Show-Preflight($checks) {
  Write-Host ''
  Write-Host 'Preflight — Windows release readiness' -ForegroundColor Cyan
  Write-Host '-------------------------------------'
  foreach ($c in $checks) {
    $mark = switch ($c.Status) { 'OK' { '[ OK ]' } 'WARN' { '[WARN]' } 'FAIL' { '[FAIL]' } }
    $color = switch ($c.Status) { 'OK' { 'Green' } 'WARN' { 'Yellow' } 'FAIL' { 'Red' } }
    $line = '{0}  {1}' -f $mark, $c.Name
    if ($c.Detail) { $line += "  ($($c.Detail))" }
    Write-Host $line -ForegroundColor $color
    if ($c.Status -ne 'OK' -and $c.Fix) { Write-Host "         fix: $($c.Fix)" -ForegroundColor DarkGray }
  }
  $fails = @($checks | Where-Object { $_.Status -eq 'FAIL' }).Count
  $warns = @($checks | Where-Object { $_.Status -eq 'WARN' }).Count
  Write-Host ''
  Write-Host ("Summary: {0} ok, {1} warning(s), {2} blocking." -f
    @($checks | Where-Object { $_.Status -eq 'OK' }).Count, $warns, $fails)
  return $fails
}

# --- Locate repo root + load .env --------------------------------------------
$Root = Split-Path -Parent $PSScriptRoot
Set-Location $Root

if (Test-Path '.env') {
  Get-Content '.env' | ForEach-Object {
    $line = $_.Trim()
    if ($line -and -not $line.StartsWith('#')) {
      $idx = $line.IndexOf('=')
      if ($idx -gt 0) {
        $k = $line.Substring(0, $idx).Trim()
        $v = $line.Substring($idx + 1).Trim().Trim('"')
        [Environment]::SetEnvironmentVariable($k, $v, 'Process')
      }
    }
  }
}

# --- Preflight dispatch ------------------------------------------------------
if ($Preflight) {
  $fails = Show-Preflight (Invoke-Preflight -ForPublish $true)
  if ($fails -gt 0) {
    Write-Host ''
    Write-Host 'Not ready: fix the [FAIL] items above, then re-run -Preflight.' -ForegroundColor Red
    exit 1
  }
  Write-Host ''
  Write-Host 'Ready. Run:  .\scripts\release-win.ps1 -Publish' -ForegroundColor Green
  exit 0
}

# A normal build runs the same checklist first and aborts on any blocker.
$fails = Show-Preflight (Invoke-Preflight -ForPublish ([bool]$Publish))
if ($fails -gt 0) {
  Die 'Preflight found blocking issues (see [FAIL] above). Fix them, then re-run (or run -Preflight to recheck).'
}

# --- Read version (owned by macOS) + repo ------------------------------------
$Version = (Get-Content 'src-tauri/tauri.conf.json' -Raw | ConvertFrom-Json).version
$Tag = "v$Version"
$Repo = (gh repo view --json nameWithOwner -q .nameWithOwner 2>$null)
if (-not $Repo) { $Repo = 'dennisrongo/agent-status' }
$ManifestPath = 'updater/latest.json'

Write-Host ''
Write-Host "==> Following macOS release $Tag on $Repo (building Windows NSIS for $Version)"

# --- Build (NSIS only, NO build-time signing) --------------------------------
# We DISABLE createUpdaterArtifacts for the build so `tauri build` never signs.
# Why: its build-time signer reads the key password from
# $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD and, when that is absent, blocks on an
# interactive CONSOLE prompt ("Decrypting updater signing key, expect a prompt
# for password") that hangs any headless/background run forever. Windows also
# can't hold a present-but-empty env var (setting one to "" deletes it), so an
# empty key password is impossible to pass through the environment. Instead we
# build only the .exe, zip it, and sign the zip with the signer CLI below, which
# takes the password as an explicit -p argument (empty or not) — no prompt.
$NoUpdaterCfg = Join-Path ([System.IO.Path]::GetTempPath()) ("relwin-noupdater-{0}.json" -f $PID)
'{ "bundle": { "createUpdaterArtifacts": false } }' | Set-Content -LiteralPath $NoUpdaterCfg -Encoding ascii
try {
  npx tauri build --bundles nsis -c "$NoUpdaterCfg"
  Assert-LastExit 'tauri build'
} finally {
  Remove-Item -LiteralPath $NoUpdaterCfg -ErrorAction SilentlyContinue
}

$NsisDir = 'src-tauri/target/release/bundle/nsis'
# Match THIS version's installer explicitly. A stale exe from a prior version may
# still sit in the bundle dir, and a bare *_x64-setup.exe glob sorts alphabetically
# (0.2.1 before 0.3.0) — picking the wrong, older payload to sign and upload.
$Exe = Get-ChildItem "$NsisDir/*_${Version}_x64-setup.exe" -ErrorAction SilentlyContinue | Select-Object -First 1
if (-not $Exe) { Die "No NSIS installer for v$Version under $NsisDir (expected *_${Version}_x64-setup.exe)." }

# --- Build the updater payload (.nsis.zip) and sign it ourselves -------------
# tauri-bundler names the payload <installer>.nsis.zip; the updater extracts it
# and runs the first *.exe inside. We reproduce that, then sign with the signer
# CLI (-f file / -k inline, -p password). This is the ONE place the key is used,
# and -p makes an empty password work non-interactively.
$ZipPath = ($Exe.FullName -replace '\.exe$', '.nsis.zip')
Remove-Item -LiteralPath $ZipPath -ErrorAction SilentlyContinue
Remove-Item -LiteralPath "$ZipPath.sig" -ErrorAction SilentlyContinue
# The payload MUST be a STORED (uncompressed) zip. The tauri-plugin-updater zip
# reader is compiled without the deflate feature, so a DEFLATE entry fails on the
# client with "unsupported Zip archive: Compression method not supported" -- which
# is exactly what tauri-bundler avoids by storing the installer uncompressed.
# PowerShell's Compress-Archive can't produce a STORED zip (on PS 5.1 / .NET
# Framework, even -CompressionLevel NoCompression tags entries as DEFLATE), so we
# use Windows' bundled bsdtar, which can force method 0. (The .exe is already
# LZMA-compressed by NSIS, so storing it costs ~nothing.)
$Bsdtar = Join-Path $env:SystemRoot 'System32\tar.exe'
if (-not (Test-Path -LiteralPath $Bsdtar)) { Die "Windows bsdtar not found at $Bsdtar - required to build a STORED updater zip the client can extract." }
& $Bsdtar -a -cf $ZipPath --options zip:compression=store -C $Exe.DirectoryName $Exe.Name
Assert-LastExit 'bsdtar (stored zip)'

$SignKey = $env:TAURI_SIGNING_PRIVATE_KEY
$SignPw  = $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD; if ($null -eq $SignPw) { $SignPw = '' }
$signOut = Invoke-SignerSign -Key $SignKey -Password $SignPw -File $ZipPath
if ($LASTEXITCODE -ne 0) { Write-Host ($signOut | Out-String); Die 'tauri signer sign failed.' }

$Zip = Get-Item -LiteralPath $ZipPath
$Sig = Get-Item -LiteralPath "$ZipPath.sig" -ErrorAction SilentlyContinue
if (-not $Sig) { Die "Signing produced no $($Zip.Name).sig - check TAURI_SIGNING_PRIVATE_KEY / TAURI_SIGNING_PRIVATE_KEY_PASSWORD." }

Write-Host ''
Write-Host '==> Built + signed:'
Write-Host "    $($Exe.FullName)"
Write-Host "    $($Zip.FullName)"
Write-Host "    $($Sig.FullName)"

# --- Merge windows-x86_64 into the shared manifest (preserves darwin) --------
# GitHub rewrites spaces in asset names to dots — match that in the URL.
$AssetName = $Zip.Name -replace ' ', '.'
$Url = "https://github.com/$Repo/releases/download/$Tag/$AssetName"

node scripts/merge-manifest.mjs `
  --manifest $ManifestPath `
  --version $Version `
  --platforms windows-x86_64 `
  --sig-file $Sig.FullName `
  --url $Url
Assert-LastExit 'merge-manifest'

Write-Host ''
Write-Host "==> Merged windows-x86_64 into $ManifestPath (darwin entries preserved)."
Write-Host '    Commit + push this file so the manifest in git matches what is published.'

# --- Publish (opt-in): upload assets into the Mac's release ------------------
if ($Publish) {
  Write-Host ''
  Write-Host "==> Uploading Windows assets into release $Tag (notes left untouched)"
  # Version-less convenience copy so the website can link to a stable URL
  # (…/releases/latest/download/Agent-Usage-Monitor-Windows-setup.exe) that always
  # resolves to the newest installer without editing HTML each release. It's a
  # byte-identical copy of the built .exe, for human downloads only — the
  # auto-updater still uses the versioned .nsis.zip.
  $StableExe = Join-Path $Exe.DirectoryName 'Agent-Usage-Monitor-Windows-setup.exe'
  Copy-Item -LiteralPath $Exe.FullName -Destination $StableExe -Force

  $uploads = @()
  if ($Exe) { $uploads += $Exe.FullName }
  $uploads += $StableExe, $Zip.FullName, $Sig.FullName, $ManifestPath
  gh release upload $Tag --clobber @uploads
  Assert-LastExit 'gh release upload'

  Write-Host ''
  Write-Host '==> Verifying the updater endpoint serves the Windows entry'
  try {
    $live = Invoke-RestMethod "https://github.com/$Repo/releases/latest/download/latest.json"
    $plats = @($live.platforms.PSObject.Properties.Name) -join ', '
    Write-Host "    latest.json -> v$($live.version); platforms: $plats"
    if (-not ($live.platforms.PSObject.Properties.Name -contains 'windows-x86_64')) {
      Write-Warning 'windows-x86_64 not present in the live manifest yet (CDN cache?) — re-check shortly.'
    }
  } catch {
    Write-Warning "Could not fetch the live manifest: $($_.Exception.Message)"
  }
} else {
  Write-Host ''
  Write-Host "Not published. Re-run with -Publish to upload into release $Tag,"
  Write-Host 'or upload the artifacts above to that release manually.'
}
