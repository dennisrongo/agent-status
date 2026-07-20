//! Process PATH repair for GUI-launched apps.
//!
//! macOS GUI apps inherit a minimal PATH (`/usr/bin:/bin:/usr/sbin:/sbin`)
//! and never source `~/.zshrc`/`~/.zshenv`, so nvm/fnm/Volta/Homebrew
//! binaries are invisible to subprocess spawns — `Command::new("npm")`
//! resolves against that minimal PATH and fails. Windows GUI apps usually
//! inherit a correct PATH (node installers write to system PATH), but
//! portable node managers (Volta, nvm-windows) may land outside it.
//!
//! `fix_login_path()` rebuilds the canonical PATH once at startup by
//! spawning the user's login shell (Unix) or augmenting with known node
//! bin dirs (Windows), then merges the result into the process env.
//! Subsequent `Command::new(...)` calls inherit the corrected PATH, so
//! [`crate::vendors::alibaba::install`] and `find_cli` work unchanged.

use std::sync::OnceLock;

/// Guards `fix_login_path` so the harvest runs at most once per process,
/// even if the function is called from multiple sites.
static FIXED: OnceLock<()> = OnceLock::new();

/// Deadline for the login-shell harvest. A healthy `~/.zshrc` finishes in a
/// few milliseconds; 3s leaves generous slack for slow setups while never
/// hanging the app launch on a broken shell init. Unix-only — Windows
/// augments PATH from known dirs without spawning a shell.
#[cfg(unix)]
const HARVEST_DEADLINE: std::time::Duration = std::time::Duration::from_secs(3);

/// Rebuild the process PATH from the user's login environment. Idempotent —
/// safe to call repeatedly; only the first call does work. On any failure
/// the inherited PATH is left untouched and a warning is logged.
///
/// Should be called once at startup, before any vendor code spawns
/// subprocesses. After this returns, the process PATH includes the dirs a
/// login shell would have set up (nvm, fnm, Volta, Homebrew, etc.).
pub fn fix_login_path() {
    if FIXED.get().is_some() {
        return;
    }
    // Mark fixed eagerly so a concurrent caller (unlikely at startup, but
    // defensive) doesn't also enter the harvest.
    let _ = FIXED.set(());

    #[cfg(unix)]
    {
        let Some(shell) = login_shell() else {
            tracing::warn!("no usable login shell found; keeping inherited PATH");
            return;
        };
        match harvest_path_from_shell(&shell) {
            Ok(harvested) => {
                let current = std::env::var("PATH").unwrap_or_default();
                let merged = merge_paths(&current, &harvested);
                if merged != current {
                    std::env::set_var("PATH", merged);
                }
            }
            Err(e) => tracing::warn!("could not harvest login PATH ({e}); keeping inherited PATH"),
        }
    }

    #[cfg(windows)]
    augment_windows_path();
}

/// Pick the shell to spawn for PATH harvesting. Prefers `$SHELL`, then
/// common macOS/Linux defaults. Returns `None` only on platforms with no
/// discoverable shell.
#[cfg(unix)]
fn login_shell() -> Option<std::path::PathBuf> {
    if let Some(s) = std::env::var_os("SHELL") {
        if !s.is_empty() && std::path::Path::new(&s).is_file() {
            return Some(std::path::PathBuf::from(s));
        }
    }
    for candidate in ["/bin/zsh", "/bin/bash"] {
        if std::path::Path::new(candidate).is_file() {
            return Some(std::path::PathBuf::from(candidate));
        }
    }
    None
}

/// Spawn `<shell> -lic 'printf %s "$PATH"'` with a hard deadline and return
/// the captured PATH.
///
/// `-l` (login) sources `~/.zprofile`/`~/.zlogin`; `-i` (interactive) is
/// required to pick up `~/.zshrc`, which is where most users (and the
/// official nvm installer) put their nvm/fnm init. Stdin is `/dev/null` so
/// the shell can't block on tty input; stderr is discarded. Despite `-i`,
/// zsh prints nothing to stdout beyond our `printf` because there is no
/// tty to draw a prompt on.
#[cfg(unix)]
fn harvest_path_from_shell(shell: &std::path::Path) -> Result<String, String> {
    use std::io::Read;
    use std::process::{Command, Stdio};

    let mut cmd = Command::new(shell);
    cmd.args(["-lic", "printf %s \"$PATH\""])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("spawn {shell:?}: {e}"))?;

    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if start.elapsed() >= HARVEST_DEADLINE {
                    let _ = child.kill();
                    return Err(format!(
                        "login shell {:?} didn't exit within {:?}",
                        shell, HARVEST_DEADLINE
                    ));
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(e) => return Err(format!("wait: {e}")),
        }
    }

    let mut out = String::new();
    child
        .stdout
        .take()
        .ok_or_else(|| "missing stdout pipe".to_string())?
        .read_to_string(&mut out)
        .map_err(|e| format!("read stdout: {e}"))?;

    let trimmed = out.trim().to_string();
    if trimmed.is_empty() {
        return Err("login shell printed empty PATH".to_string());
    }
    Ok(trimmed)
}

/// Merge two PATH strings, deduplicating entries and preserving order.
///
/// Entries from `shell_path` come first (the login environment wins over
/// the GUI PATH — nvm/fnm dirs should take precedence over a stale system
/// node). Entries from `current` that aren't already present are appended.
/// Empty entries are dropped. `sep` is `:` on Unix, `;` on Windows.
fn merge_paths_with(current: &str, shell_path: &str, sep: char) -> String {
    use std::collections::HashSet;

    let mut seen: HashSet<&str> = HashSet::new();
    let mut ordered: Vec<&str> = Vec::new();
    for entry in shell_path.split(sep).chain(current.split(sep)) {
        if !entry.is_empty() && seen.insert(entry) {
            ordered.push(entry);
        }
    }

    let mut out = String::with_capacity(shell_path.len() + current.len());
    for (i, entry) in ordered.iter().enumerate() {
        if i > 0 {
            out.push(sep);
        }
        out.push_str(entry);
    }
    out
}

/// Platform-aware wrapper around [`merge_paths_with`] — picks the right
/// separator for the host. Public so callers and tests don't need to know
/// the separator.
pub fn merge_paths(current: &str, shell_path: &str) -> String {
    merge_paths_with(current, shell_path, if cfg!(windows) { ';' } else { ':' })
}

/// Augment the Windows process PATH with known node/npm bin directories if
/// they exist and aren't already present. No-op when everything is already
/// on PATH (the common case, since node installers write to system PATH).
#[cfg(windows)]
fn augment_windows_path() {
    use std::path::PathBuf;

    let mut candidates: Vec<PathBuf> = Vec::new();

    // Stock node: npm global installs land in %APPDATA%\npm.
    if let Some(appdata) = std::env::var_os("APPDATA") {
        candidates.push(PathBuf::from(&appdata).join("npm"));
    }
    // Volta: %LOCALAPPDATA%\Volta\bin.
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        candidates.push(PathBuf::from(&local).join("Volta").join("bin"));
    }
    // nvm-windows: the active node version is exposed via the %NVM_SYMLINK%
    // directory (defaults to a `nodejs` folder under Program Files).
    if let Some(symlink) = std::env::var_os("NVM_SYMLINK") {
        candidates.push(PathBuf::from(&symlink));
    } else if let Some(pf) = std::env::var_os("ProgramFiles") {
        candidates.push(PathBuf::from(&pf).join("nodejs"));
    }

    let existing: Vec<PathBuf> = std::env::var_os("PATH")
        .as_deref()
        .map(|p| std::env::split_paths(p).map(PathBuf::from).collect())
        .unwrap_or_default();

    let mut to_prepend: Vec<PathBuf> = candidates
        .into_iter()
        .filter(|c| c.is_dir() && !existing.iter().any(|e| e == c))
        .collect();

    if to_prepend.is_empty() {
        return;
    }

    to_prepend.extend(existing);
    if let Ok(joined) = std::env::join_paths(to_prepend) {
        std::env::set_var("PATH", joined);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_prepends_shell_entries_and_dedups() {
        // Shell entries come first; duplicates shared with `current` are not
        // repeated.
        let got = merge_paths_with("/usr/bin:/bin", "/nvm/bin:/usr/bin", ':');
        assert_eq!(got, "/nvm/bin:/usr/bin:/bin");
    }

    #[test]
    fn merge_preserves_order_within_each_source() {
        let got = merge_paths_with(
            "/usr/bin:/bin:/usr/sbin",
            "/nvm/bin:/opt/homebrew/bin",
            ':',
        );
        assert_eq!(got, "/nvm/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin");
    }

    #[test]
    fn merge_drops_empty_entries() {
        // Stray colons (common in shell PATHs from rc-file concatenation)
        // should not produce empty segments.
        let got = merge_paths_with(":/usr/bin:", ":/nvm/bin:", ':');
        assert_eq!(got, "/nvm/bin:/usr/bin");
    }

    #[test]
    fn merge_with_empty_current_returns_shell() {
        assert_eq!(merge_paths_with("", "/nvm/bin:/usr/bin", ':'), "/nvm/bin:/usr/bin");
    }

    #[test]
    fn merge_with_empty_shell_returns_current() {
        assert_eq!(merge_paths_with("/usr/bin:/bin", "", ':'), "/usr/bin:/bin");
    }

    #[test]
    fn merge_both_empty_returns_empty() {
        assert_eq!(merge_paths_with("", "", ':'), "");
    }

    #[test]
    fn merge_uses_semicolon_separator_on_windows_form() {
        // Exercises the Windows separator explicitly so the logic is
        // covered regardless of the host running the tests.
        let got = merge_paths_with(
            r"C:\Windows\System32;C:\Windows",
            r"C:\Users\me\AppData\Roaming\npm",
            ';',
        );
        assert_eq!(
            got,
            r"C:\Users\me\AppData\Roaming\npm;C:\Windows\System32;C:\Windows"
        );
    }

    #[test]
    fn merge_is_a_noop_when_paths_already_equal() {
        let p = "/nvm/bin:/usr/bin:/bin";
        assert_eq!(merge_paths_with(p, p, ':'), p);
    }

    #[test]
    fn merge_does_not_swap_position_of_shared_suffix() {
        // If `current` has an entry the shell also has, it stays in the
        // shell-position (first occurrence wins).
        let got = merge_paths_with("/shared:/only_in_current", "/shared:/shell_only", ':');
        assert_eq!(got, "/shared:/shell_only:/only_in_current");
    }
}
