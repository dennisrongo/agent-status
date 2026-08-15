//! Alibaba Cloud Model Studio (Bailian) usage client.
//!
//! Shells out to the `bl` CLI (`bailian-cli`) to read free-tier quota and
//! usage statistics. The CLI authenticates via its own config
//! (`~/.bailian/config.json`) — no API key is stored by this app. Detection
//! checks PATH, the npm global bin directory, and common install locations.

use serde_json::Value;
use std::path::PathBuf;
use std::sync::OnceLock;

use chrono::{DateTime, Utc};

use crate::process_util::SilentCommand;
use super::{KeyVal, VendorStatus};

/// Token Plan console API path (undocumented — reverse-engineered from the
/// Alibaba Cloud console's DevTools network tab).
const TOKEN_PLAN_USAGE_API: &str = "zeldaHttp.apikeyMgr./tokenplan/personal/api/v2/usage";

/// Cached npm global prefix — the `npm config get prefix` subprocess is
/// expensive, so we run it at most once per process lifetime. The directory
/// itself doesn't change mid-session; we still stat for `bl` on every call so
/// a fresh install is picked up.
static NPM_PREFIX: OnceLock<Option<PathBuf>> = OnceLock::new();

fn npm_global_prefix() -> Option<PathBuf> {
    NPM_PREFIX
        .get_or_init(|| {
            let out = npm_command()
                .args(["config", "get", "prefix"])
                .output()
                .ok()?;
            if !out.status.success() {
                return None;
            }
            let prefix = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if prefix.is_empty() {
                return None;
            }
            Some(PathBuf::from(prefix))
        })
        .clone()
}

/// Locate the `bl` (Bailian CLI) binary. Checks PATH first, then the npm
/// global prefix, then well-known install directories. Returns the path so
/// callers can invoke it directly even when it isn't on PATH.
///
/// On Windows the candidate list skips the extensionless `bl` (an sh script
/// npm ships for Git Bash) — CreateProcess can't run it and it sorts before
/// `bl.cmd`. On Unix the extensionless `bl` is the real binary.
pub fn find_cli() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    const NAMES: &[&str] = &["bl.exe", "bl.cmd"];
    #[cfg(not(target_os = "windows"))]
    const NAMES: &[&str] = &["bl"];

    // 1. PATH scan (covers most installs).
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            for name in NAMES {
                let candidate = dir.join(name);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }

    // 2. npm global prefix (cached — the subprocess runs at most once).
    if let Some(base) = npm_global_prefix() {
        // npm puts binaries in <prefix>/ on Windows, <prefix>/bin/ on Unix.
        for name in NAMES {
            let candidate = base.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        #[cfg(not(target_os = "windows"))]
        {
            let bin = base.join("bin");
            if bin.join("bl").is_file() {
                return Some(bin.join("bl"));
            }
        }
    }

    // 3. Well-known fallback: %APPDATA%\npm on Windows.
    #[cfg(target_os = "windows")]
    if let Some(appdata) = std::env::var_os("APPDATA") {
        let candidate = PathBuf::from(appdata).join("npm").join("bl.cmd");
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    None
}

/// Whether the `bl` (Bailian CLI) binary is reachable.
pub fn cli_on_path() -> bool {
    find_cli().is_some()
}

/// Build a Command for npm, handling Windows .cmd wrappers.
/// On Windows, npm ships as `npm.cmd` — a batch shim that CreateProcess can't
/// execute directly. Route through `cmd.exe /C` so the spawn succeeds.
fn npm_command() -> std::process::Command {
    #[cfg(target_os = "windows")]
    {
        let mut cmd = std::process::Command::new("cmd");
        cmd.arg("/C").arg("npm").silent();
        return cmd;
    }
    #[cfg(not(target_os = "windows"))]
    {
        let mut cmd = std::process::Command::new("npm");
        cmd.silent();
        cmd
    }
}

/// Build a Command for the Bailian CLI, handling Windows .cmd wrappers.
/// On Windows, npm global binaries are .cmd shims that CreateProcess can't
/// execute directly — they must go through cmd.exe /C. `.silent()` suppresses
/// the console window on every `bl` invocation (auth status, login, and the
/// usage fetches that fire on every refresh tick).
fn bl_command(cli: &std::path::Path) -> std::process::Command {
    #[cfg(target_os = "windows")]
    {
        let ext = cli.extension().and_then(|e| e.to_str()).unwrap_or("");
        if ext.eq_ignore_ascii_case("cmd") || ext.eq_ignore_ascii_case("bat") {
            let mut cmd = std::process::Command::new("cmd");
            cmd.arg("/C").arg(cli).silent();
            return cmd;
        }
    }
    let mut cmd = std::process::Command::new(cli);
    cmd.silent();
    cmd
}

/// What the Settings UI shows about the Bailian CLI: installed? authenticated?
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CliStatus {
    pub installed: bool,
    pub authenticated: bool,
    /// Masked API key or console token hint (never the full credential).
    pub auth_hint: Option<String>,
    /// OpenAPI AK/SK credentials are configured — the CLI can auto-refresh
    /// the console session token when it expires, so the user won't need to
    /// re-login manually. Without these, the console session expires after a
    /// few hours and requires `bl auth login --console` again.
    pub has_open_api: bool,
}

/// Query the CLI's own auth status (`bl auth status --output json`).
pub fn auth_status() -> CliStatus {
    let Some(cli) = find_cli() else {
        return CliStatus { installed: false, authenticated: false, auth_hint: None, has_open_api: false };
    };

    let out = bl_command(&cli)
        .args(["auth", "status", "--output", "json"])
        .output();

    let Ok(out) = out else {
        return CliStatus { installed: true, authenticated: false, auth_hint: None, has_open_api: false };
    };

    let Ok(v) = serde_json::from_slice::<Value>(&out.stdout) else {
        return CliStatus { installed: true, authenticated: false, auth_hint: None, has_open_api: false };
    };

    // The AK/SK may only be visible via the config file the CLI reports (the
    // status JSON doesn't always carry it) — read that as the fallback.
    let config_has_ak = v
        .get("config_file")
        .and_then(|f| f.as_str())
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|cfg| serde_json::from_str::<Value>(&cfg).ok())
        .and_then(|cfg| cfg.get("access_key_id").and_then(|k| k.as_str()).map(|k| !k.is_empty()))
        .unwrap_or(false);

    interpret_auth_status(&v, config_has_ak)
}

/// Pure interpretation of the `bl auth status --output json` payload, split
/// from the subprocess so the detection logic is unit-testable. `config_has_ak`
/// carries the config-file fallback read.
fn interpret_auth_status(v: &Value, config_has_ak: bool) -> CliStatus {
    let authenticated = v.get("authenticated").and_then(|a| a.as_bool()).unwrap_or(false);
    // Build a hint from the masked key or console token — never the real value.
    let auth_hint = v
        .get("console")
        .and_then(|c| c.get("masked"))
        .and_then(|m| m.as_str())
        .map(|m| format!("console · {m}"))
        .or_else(|| {
            v.get("api_key")
                .and_then(|k| k.get("masked"))
                .and_then(|m| m.as_str())
                .map(|m| format!("api key · {m}"))
        });

    // OpenAPI AK/SK lets the CLI auto-refresh the console session token. The
    // current CLI reports them under `openapi`; `open_api` and a top-level
    // `access_key_id` are accepted for other builds, and `config_has_ak`
    // covers builds that report neither.
    let has_open_api = v
        .get("openapi")
        .or_else(|| v.get("open_api"))
        .is_some_and(|v| !v.is_null())
        || v.get("access_key_id").is_some_and(|v| !v.is_null())
        || config_has_ak;

    CliStatus { installed: true, authenticated, auth_hint, has_open_api }
}

/// Run `bl auth login --console` to authenticate via the browser. The CLI
/// opens a browser for the user to complete the OAuth flow; this blocks until
/// they finish (or cancel). Returns a human-readable result.
pub fn login() -> Result<String, String> {
    let Some(cli) = find_cli() else {
        return Err("Bailian CLI not found — install it first.".to_string());
    };

    let out = bl_command(&cli)
        .args(["auth", "login", "--console"])
        .output()
        .map_err(|e| format!("spawn: {e}"))?;

    if out.status.success() {
        Ok("Authenticated with Alibaba. Usage will appear on the next refresh.".to_string())
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let detail = if !stderr.trim().is_empty() {
            stderr.trim().to_string()
        } else if !stdout.trim().is_empty() {
            stdout.trim().to_string()
        } else {
            format!("exit code {}", out.status.code().unwrap_or(-1))
        };
        Err(format!("Login failed: {detail}"))
    }
}

/// Run `bl auth logout` to clear all stored credentials (API key, console
/// session, and OpenAPI AK/SK). The CLI removes them from its config file.
pub fn logout() -> Result<String, String> {
    let Some(cli) = find_cli() else {
        return Err("Bailian CLI not found.".to_string());
    };

    let out = bl_command(&cli)
        .args(["auth", "logout"])
        .output()
        .map_err(|e| format!("spawn: {e}"))?;

    if out.status.success() {
        Ok("Disconnected from Alibaba.".to_string())
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let detail = if !stderr.trim().is_empty() {
            stderr.trim().to_string()
        } else if !stdout.trim().is_empty() {
            stdout.trim().to_string()
        } else {
            format!("exit code {}", out.status.code().unwrap_or(-1))
        };
        Err(format!("Logout failed: {detail}"))
    }
}

/// Run `bl auth login --open-api` with the given AK/SK to enable automatic
/// console session refresh. The CLI stores the credentials in its own config
/// file — this app never persists them.
pub fn set_open_api(access_key_id: &str, access_key_secret: &str) -> Result<String, String> {
    let Some(cli) = find_cli() else {
        return Err("Bailian CLI not found — install it first.".to_string());
    };

    let out = bl_command(&cli)
        .args([
            "auth", "login", "--open-api",
            "--access-key-id", access_key_id,
            "--access-key-secret", access_key_secret,
        ])
        .output()
        .map_err(|e| format!("spawn: {e}"))?;

    if out.status.success() {
        Ok("OpenAPI credentials saved — the CLI will auto-refresh your session.".to_string())
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let detail = if !stderr.trim().is_empty() {
            stderr.trim().to_string()
        } else if !stdout.trim().is_empty() {
            stdout.trim().to_string()
        } else {
            format!("exit code {}", out.status.code().unwrap_or(-1))
        };
        Err(format!("Failed to save OpenAPI credentials: {detail}"))
    }
}

/// Install the Bailian CLI globally via npm. Returns a human-readable result.
pub fn install() -> Result<String, String> {
    // Verify npm is available first.
    let npm_ok = npm_command()
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !npm_ok {
        return Err(
            "npm not found — install Node.js (≥ 22.12) first: https://nodejs.org".to_string(),
        );
    }

    let out = npm_command()
        .args(["install", "-g", "bailian-cli"])
        .output()
        .map_err(|e| format!("npm spawn failed: {e}"))?;

    if out.status.success() {
        // Verify the binary is now reachable.
        if find_cli().is_some() {
            Ok("Bailian CLI installed. Run `bl auth login --console` in a terminal to authenticate.".to_string())
        } else {
            Ok("Installed, but `bl` isn't on PATH yet — restart the app or add the npm global bin to PATH.".to_string())
        }
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr);
        Err(format!("npm install failed: {}", stderr.trim()))
    }
}

/// Fetch the Token Plan 5h/7d quota via `bl console call` (the same
/// percentages the Alibaba Cloud console shows). Called from a blocking task.
///
/// Auth detection: the console call needs a valid *console* session, which is
/// a separate credential from the API key `bl auth status` reports. The
/// session can expire (or never have been logged in) while `auth_status()`
/// still says `authenticated: true`. Since only a usage call discovers this,
/// `fetch()` is the authority — when the command fails with the
/// console-expired error we return a terminal `auth_expired` status.
///
/// Session renewal: the CLI renews the console session IN PLACE during
/// `bl console call` when OpenAPI AK/SK are configured (a signed
/// `GenerateCLIAccessToken` OpenAPI call, written back to its own config) —
/// but it doesn't retry that renewal when the signed call itself fails
/// transiently. So on an expired-session error with AK/SK present we invoke
/// the console call once more, giving the CLI's own renewal a second shot
/// before declaring the session dead (the same outcome as Kimi's in-place
/// refresh, using the CLI's machinery rather than re-implementing Alibaba's
/// RPC request signing). Without AK/SK a retry can only fail the same way, so
/// we skip it.
pub fn fetch() -> VendorStatus {
    let Some(cli) = find_cli() else {
        return VendorStatus::not_configured();
    };
    match console_call(&cli) {
        Ok(plan) => parse(&plan, Utc::now()),
        Err(e) if e.is_console_expired() => {
            if auth_status().has_open_api {
                if let Ok(plan) = console_call(&cli) {
                    return parse(&plan, Utc::now());
                }
            }
            expired_status(&e)
        }
        Err(e) => VendorStatus::failed(format!("bl console call: {e}")),
    }
}

/// One `bl console call` for the Token Plan usage API.
fn console_call(cli: &std::path::Path) -> Result<Value, BlError> {
    run_bl(cli, &[
        "console", "call",
        "--api", TOKEN_PLAN_USAGE_API,
        "--data", "{}",
        "--output", "json",
    ])
}

/// Structured error from a `bl` invocation: the CLI's JSON
/// `{ "error": { code, message, hint } }` (it writes this to **stderr**, not
/// stdout, on a non-zero exit), or a spawn/parse failure. Carrying the parsed
/// fields lets `fetch()` distinguish a console-session expiry — which is
/// terminal until `bl auth login --console` re-authenticates — from a
/// transient network blip, instead of treating every failure the same.
struct BlError {
    code: Option<i64>,
    message: String,
    hint: Option<String>,
}

impl BlError {
    /// Whether this is the "console session expired / not logged in" auth
    /// failure. Code 3 is the CLI's credential error; the message match is a
    /// backstop in case a future CLI build changes the code. `bl auth status`
    /// can't see this state on its own — it still reports `authenticated:
    /// true` as long as a separate API key is present — so only a usage call
    /// discovers it.
    fn is_console_expired(&self) -> bool {
        self.code == Some(3)
            || self
                .message
                .contains("Console session is not logged in or has expired")
    }
}

impl std::fmt::Display for BlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let hint = self.hint.as_deref().filter(|h| !h.is_empty());
        match (self.code, hint) {
            (Some(c), Some(h)) => write!(f, "exit code {c}: {} ({h})", self.message),
            (Some(c), None) => write!(f, "exit code {c}: {}", self.message),
            (None, Some(h)) => write!(f, "{} ({h})", self.message),
            (None, None) => write!(f, "{}", self.message),
        }
    }
}

/// Parse the CLI's structured error JSON (`{ "error": { code, message, hint } }`)
/// into a `BlError`, if present. Pure — exercised by tests without shelling out.
/// Accepts both the wrapped (`{ "error": { ... } }`) and flat (`{ "code", ... }`)
/// shapes a CLI build might emit.
fn parse_bl_error(json: &str) -> Option<BlError> {
    let v = serde_json::from_str::<Value>(json).ok()?;
    let err = v.get("error").unwrap_or(&v);
    let message = err.get("message").and_then(|m| m.as_str())?.to_string();
    Some(BlError {
        code: err.get("code").and_then(|c| c.as_i64()),
        message,
        hint: err.get("hint").and_then(|h| h.as_str()).map(|h| h.to_string()),
    })
}

fn run_bl(cli: &std::path::Path, args: &[&str]) -> Result<Value, BlError> {
    let out = bl_command(cli)
        .args(args)
        .output()
        .map_err(|e| BlError { code: None, message: format!("spawn: {e}"), hint: None })?;

    if !out.status.success() {
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        // The CLI writes structured JSON errors to stderr (and sometimes
        // stdout) even on a non-zero exit — parse whichever has it so we keep
        // the hint and the machine-readable code.
        if let Some(e) = parse_bl_error(&stdout).or_else(|| parse_bl_error(&stderr)) {
            return Err(BlError {
                code: e.code.or_else(|| out.status.code().map(|c| c as i64)),
                ..e
            });
        }
        let raw = if !stderr.trim().is_empty() {
            stderr.trim().to_string()
        } else {
            stdout.trim().to_string()
        };
        return Err(BlError {
            code: out.status.code().map(|c| c as i64),
            message: raw,
            hint: None,
        });
    }

    serde_json::from_str(&String::from_utf8_lossy(&out.stdout))
        .map_err(|e| BlError { code: None, message: format!("invalid JSON: {e}"), hint: None })
}

/// Build the terminal "console session expired" status. The CLI's hint (the
/// `Run \`bl auth login --console\` …` line) is the actionable part — surface
/// it verbatim in `error` so the Overview and Settings can show it.
fn expired_status(e: &BlError) -> VendorStatus {
    VendorStatus {
        configured: true,
        ok: false,
        error: e.hint.clone().or_else(|| Some(e.to_string())),
        primary: "—".to_string(),
        secondary: "session expired".to_string(),
        detail: Vec::new(),
        auth_expired: true,
    }
}

/// Pure parser: extracts the Token Plan 5h/7d quota meters from the console
/// API response. `now` is passed in so reset countdowns are deterministic
/// (matching GLM's pattern).
pub fn parse(plan: &Value, now: DateTime<Utc>) -> VendorStatus {
    let mut detail: Vec<KeyVal> = Vec::new();

    let envelope = datav2_payload(plan);
    if let Some(d) = envelope {
        let pct_5h = d.get("per5HourPercentage").and_then(value_as_f64);
        let pct_7d = d.get("per1WeekPercentage").and_then(value_as_f64);
        let reset_5h = d.get("per5HourResetTime").and_then(value_as_f64);
        let reset_7d = d.get("per1WeekResetTime").and_then(value_as_f64);

        if let Some(p) = pct_5h {
            let reset_label = reset_5h
                .and_then(|ms| countdown_ms(ms as i64, now))
                .map(|r| format!("resets in {r}"))
                .unwrap_or_default();
            detail.push(KeyVal::meter("5 hours", reset_label, p * 100.0));
        }
        if let Some(p) = pct_7d {
            let reset_label = reset_7d
                .and_then(|ms| countdown_ms(ms as i64, now))
                .map(|r| format!("resets in {r}"))
                .unwrap_or_default();
            detail.push(KeyVal::meter("7 days", reset_label, p * 100.0));
        }
    }

    if detail.is_empty() {
        // A `data` object without the DataV2 envelope usually means the console
        // API changed shape — flag it in the secondary line instead of reading
        // as a plain "no plan data" (the Kimi `used`-field removal failed
        // exactly this silently). A null/absent `data` stays a plain no-plan
        // reading.
        let unexpected_shape =
            envelope.is_none() && plan.get("data").map(|d| d.is_object()).unwrap_or(false);
        return VendorStatus {
            configured: true,
            ok: true,
            error: None,
            primary: "—".to_string(),
            secondary: if unexpected_shape {
                "no plan data · unexpected response shape".to_string()
            } else {
                "no plan data".to_string()
            },
            detail: Vec::new(),
            auth_expired: false,
        };
    }

    let max_pct = detail
        .iter()
        .filter_map(|d| d.pct)
        .fold(0.0_f64, f64::max);

    VendorStatus {
        configured: true,
        ok: true,
        error: None,
        primary: format!("{:.0}% used", max_pct),
        secondary: "token plan".to_string(),
        detail,
        auth_expired: false,
    }
}

fn value_as_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.parse::<f64>().ok(),
        _ => None,
    }
}

/// Format a reset epoch (ms) as a compact countdown from `now`, matching
/// Claude's "4h 12m" / "2d 3h" style. Returns `None` for a past reset.
fn countdown_ms(reset_ms: i64, now: DateTime<Utc>) -> Option<String> {
    let secs = reset_ms.checked_sub(now.timestamp_millis())? / 1000;
    if secs <= 0 {
        return None;
    }
    let d = secs / 86_400;
    let h = (secs % 86_400) / 3_600;
    let m = (secs % 3_600) / 60;
    Some(if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else {
        format!("{m}m")
    })
}

/// Walk the `data.DataV2.data.data` envelope the console API wraps responses in.
fn datav2_payload(v: &Value) -> Option<&Value> {
    v.get("data")
        .and_then(|d| d.get("DataV2"))
        .and_then(|d| d.get("data"))
        .and_then(|d| d.get("data"))
}


#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn now() -> DateTime<Utc> {
        Utc::now()
    }

    #[test]
    fn parses_console_expired_error() {
        // The exact JSON the CLI writes to stderr when the console session has
        // expired — verbatim from a real `bl usage summary` exit-code-3 run.
        let json = r#"{
            "error": {
                "code": 3,
                "message": "Console session is not logged in or has expired.",
                "hint": "Run `bl auth login --console` to sign in or refresh your console session."
            }
        }"#;
        let e = parse_bl_error(json).expect("error JSON should parse");
        assert_eq!(e.code, Some(3));
        assert!(e.is_console_expired());
        let s = expired_status(&e);
        assert!(!s.ok);
        assert!(s.configured);
        assert!(s.auth_expired);
        assert_eq!(s.secondary, "session expired");
        // The hint is the actionable line — surface it verbatim.
        assert!(s.error.unwrap().contains("bl auth login --console"));
    }

    // ── auth status interpretation ──

    #[test]
    fn auth_status_detects_openapi_section_current_shape() {
        // Verbatim shape from `bl auth status --output json` (CLI 1.13.0): the
        // OpenAPI credentials live under an `openapi` key (no underscore).
        let v = json!({
            "authenticated": true,
            "config_file": "C:\\Users\\x\\.bailian\\config.json",
            "api_key": { "source": "env", "masked": "sk-s...9WXM" },
            "console": { "source": "config", "masked": "3613...7ee8" },
            "openapi": { "source": "config", "access_key_id": "LTAI...jkAo", "access_key_secret": "ylMu...fy46" }
        });
        let s = interpret_auth_status(&v, false);
        assert!(s.installed && s.authenticated && s.has_open_api);
        assert_eq!(s.auth_hint.as_deref(), Some("console · 3613...7ee8"));
    }

    #[test]
    fn auth_status_detects_legacy_and_top_level_openapi_keys() {
        // Other CLI builds may spell the section `open_api`…
        let v = json!({ "authenticated": true, "open_api": { "access_key_id": "x" } });
        assert!(interpret_auth_status(&v, false).has_open_api);
        // …or carry the AK at the top level.
        let v = json!({ "authenticated": true, "access_key_id": "x" });
        assert!(interpret_auth_status(&v, false).has_open_api);
    }

    #[test]
    fn auth_status_falls_back_to_config_file_read() {
        // Status JSON carries no OpenAPI info at all — the config-file read is
        // the only signal.
        let v = json!({ "authenticated": true });
        assert!(interpret_auth_status(&v, true).has_open_api);
        assert!(!interpret_auth_status(&v, false).has_open_api);
    }

    #[test]
    fn auth_status_null_openapi_means_not_configured() {
        let v = json!({ "authenticated": true, "openapi": null });
        assert!(!interpret_auth_status(&v, false).has_open_api);
    }

    #[test]
    fn auth_status_hint_prefers_console_then_api_key() {
        let v = json!({ "api_key": { "masked": "sk-s...9WXM" } });
        assert_eq!(interpret_auth_status(&v, false).auth_hint.as_deref(), Some("api key · sk-s...9WXM"));
        let v = json!({});
        assert!(interpret_auth_status(&v, false).auth_hint.is_none());
    }

    #[test]
    fn console_expired_detection_ignores_transient_errors() {
        // A rate-limit / network failure isn't the console-expiry error and
        // must not flip auth_expired — that would turn a transient blip into a
        // false "sign in again" prompt.
        let e = parse_bl_error(r#"{ "error": { "code": 429, "message": "Too Many Requests" } }"#)
            .expect("error JSON should parse");
        assert_eq!(e.code, Some(429));
        assert!(!e.is_console_expired());
        // Falls back to message-only display when there's no hint.
        assert_eq!(e.to_string(), "exit code 429: Too Many Requests");
    }

    #[test]
    fn parse_bl_error_handles_flat_shape() {
        // A CLI build could emit the error fields at the top level instead of
        // nested under "error" — accept that shape too.
        let e = parse_bl_error(r#"{ "code": 3, "message": "Console session is not logged in or has expired." }"#)
            .expect("flat error JSON should parse");
        assert!(e.is_console_expired());
    }

    #[test]
    fn parses_plan_quota() {
        let n = now();
        let now_ms = n.timestamp_millis();
        let plan = json!({
            "data": {
                "DataV2": {
                    "data": {
                        "data": {
                            "per5HourPercentage": 0.047,
                            "per5HourResetTime": now_ms + 4 * 3600 * 1000 + 30 * 60 * 1000,
                            "per1WeekPercentage": 0.724,
                            "per1WeekResetTime": now_ms + 6 * 24 * 3600 * 1000
                        }
                    }
                }
            }
        });
        let s = parse(&plan, n);
        assert!(s.ok);
        assert_eq!(s.primary, "72% used");
        assert_eq!(s.secondary, "token plan");
        assert_eq!(s.detail.len(), 2);
        let five_h = s.detail.iter().find(|d| d.label == "5 hours").unwrap();
        assert_eq!(five_h.pct, Some(4.7));
        assert!(five_h.value.starts_with("resets in"));
        let seven_d = s.detail.iter().find(|d| d.label == "7 days").unwrap();
        assert_eq!(seven_d.pct, Some(72.4));
        assert!(seven_d.value.starts_with("resets in"));
    }

    #[test]
    fn parses_plan_quota_real_response() {
        // Verbatim shape from `bl console call` (percentages are 0–1 scale).
        let n = now();
        let now_ms = n.timestamp_millis();
        let plan = json!({
            "data": {
                "DataV2": {
                    "data": {
                        "msg": "Success.",
                        "code": "SUCCESS",
                        "data": {
                            "per5HourPercentage": 0.06128406807503333,
                            "per1WeekResetTime": now_ms + 5 * 24 * 3600 * 1000,
                            "per5HourResetTime": now_ms + 3 * 3600 * 1000,
                            "per1WeekPercentage": 0.72473245405233
                        }
                    }
                }
            }
        });
        let s = parse(&plan, n);
        assert!(s.ok);
        assert_eq!(s.primary, "72% used");
        let five_h = &s.detail[0];
        assert_eq!(five_h.label, "5 hours");
        assert_eq!(five_h.pct, Some(6.1));
        let seven_d = &s.detail[1];
        assert_eq!(seven_d.label, "7 days");
        assert_eq!(seven_d.pct, Some(72.5));
    }

    #[test]
    fn parses_empty_envelope_as_no_plan_data() {
        let s = parse(&json!({}), now());
        assert!(s.ok);
        assert_eq!(s.primary, "—");
        assert_eq!(s.secondary, "no plan data");
        assert!(s.detail.is_empty());
    }

    #[test]
    fn parses_missing_percentage_fields() {
        // Envelope is valid but the inner data has no percentage fields.
        let plan = json!({
            "data": { "DataV2": { "data": { "data": {
                "msg": "Success."
            }}}}
        });
        let s = parse(&plan, now());
        assert!(s.ok);
        assert_eq!(s.primary, "—");
        assert!(s.detail.is_empty());
    }

    #[test]
    fn envelope_moved_surfaces_a_shape_hint() {
        // `data` exists but the DataV2 envelope is gone — likely an API shape
        // change; must not read as a plain "no plan data".
        let s = parse(&json!({ "data": { "DataV3": { "data": { "data": {} } } } }), now());
        assert!(s.ok);
        assert_eq!(s.secondary, "no plan data · unexpected response shape");
    }

    #[test]
    fn null_data_is_plain_no_plan_data() {
        let s = parse(&json!({ "data": null }), now());
        assert!(s.ok);
        assert_eq!(s.secondary, "no plan data");
    }

    #[test]
    fn parses_only_5h_when_7d_absent() {
        let n = now();
        let now_ms = n.timestamp_millis();
        let plan = json!({
            "data": { "DataV2": { "data": { "data": {
                "per5HourPercentage": 0.5,
                "per5HourResetTime": now_ms + 3600 * 1000
            }}}}
        });
        let s = parse(&plan, n);
        assert!(s.ok);
        assert_eq!(s.primary, "50% used");
        assert_eq!(s.detail.len(), 1);
        assert_eq!(s.detail[0].label, "5 hours");
    }
}
