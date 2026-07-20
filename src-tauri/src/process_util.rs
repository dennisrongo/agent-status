//! Helpers for spawning child processes without flashing a console window.
//!
//! On Windows, `std::process::Command` allocates a new console for every child
//! by default — even when the parent is a GUI app — so each `npm`, `cmd /C`,
//! `gh`, or `rundll32` invocation pops a black `cmd.exe` window on screen. This
//! is most visible right after an update relaunches the app: the per-process
//! caches (`NPM_PREFIX`, the Copilot discovered token) are cold, so the first
//! refresh tick fires several spawns back-to-back. Passing
//! `CREATE_NO_WINDOW` (`0x0800_0000`) suppresses the console for that child.
//!
//! Usage: build the `Command` as usual, then call `.silent()` before
//! `.output()` / `.spawn()`. On non-Windows targets `silent()` is a no-op, so
//! the same code is portable.
//!
//! ```
//! use crate::process_util::SilentCommand;
//! let out = std::process::Command::new("npm")
//!     .args(["config", "get", "prefix"])
//!     .silent()
//!     .output()?;
//! ```

#[cfg(windows)]
use std::os::windows::process::CommandExt;

/// `CREATE_PROCESS` flag that prevents the child from inheriting or creating a
/// visible console. See [`CommandExt::creation_flags`].
///
/// <https://learn.microsoft.com/windows/win32/procthread/process-creation-flags#CREATE_NO_WINDOW>
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Extension trait giving every `Command` a portable `.silent()` builder.
pub trait SilentCommand {
    /// On Windows, set `CREATE_NO_WINDOW` so the child doesn't pop a console.
    /// No-op elsewhere.
    fn silent(&mut self) -> &mut Self;
}

impl SilentCommand for std::process::Command {
    #[inline]
    fn silent(&mut self) -> &mut Self {
        #[cfg(windows)]
        self.creation_flags(CREATE_NO_WINDOW);
        self
    }
}
