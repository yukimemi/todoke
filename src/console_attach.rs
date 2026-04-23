//! Windows-only: attach to the parent process's console on startup and
//! rebind stdio so `println!` / `tracing` reaches the terminal that
//! launched us.
//!
//! Why: release builds use `windows_subsystem = "windows"` so explorer
//! / shortcut / file-association launches don't flash a transient
//! console window. A windows-subsystem exe starts with no attached
//! console at all, so terminal launches need us to:
//!
//! 1. `AttachConsole(ATTACH_PARENT_PROCESS)` — grab the launching
//!    terminal's console if it has one.
//! 2. For each standard handle that's currently **empty** (null),
//!    re-open `CONOUT$` / `CONIN$` and wire it in with `SetStdHandle`.
//!
//! The "currently empty" check is load-bearing. Shell redirection
//! (`todoke --version > out.txt`, `todoke | clip`) leaves a real
//! redirected handle in place even before we attach. Clobbering it
//! would break piping and redirection — so we only touch handles that
//! would otherwise be null.
//!
//! When launched from explorer `AttachConsole` returns 0 and this
//! whole function is a no-op (stdio stays null — exactly what we want).
//!
//! **Call ordering is load-bearing.** Rust's `io::stdout()` /
//! `io::stderr()` / `io::stdin()` cache the handle on first use. If
//! anything writes to stdio *before* `attach_parent_console()` runs,
//! the null handle gets cached and the `SetStdHandle` calls here
//! become no-ops from Rust's perspective. `main()` invokes this as
//! the first statement; don't introduce earlier stdio usage (panic
//! hooks, static initializers, etc.) without re-thinking this.

use std::fs::OpenOptions;
use std::os::windows::io::IntoRawHandle;

use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Console::{
    ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_ERROR_HANDLE, STD_HANDLE,
    STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle,
};

pub fn attach_parent_console() {
    // SAFETY: Win32 API call; the return value distinguishes success (1)
    // from no-parent-console (0).
    if unsafe { AttachConsole(ATTACH_PARENT_PROCESS) } == 0 {
        return;
    }
    maybe_rebind("CONOUT$", STD_OUTPUT_HANDLE, true);
    maybe_rebind("CONOUT$", STD_ERROR_HANDLE, true);
    maybe_rebind("CONIN$", STD_INPUT_HANDLE, false);
}

/// Re-open the console device and install it as `which` — but ONLY if
/// `which` is currently null. A non-null existing handle means the
/// caller redirected it (e.g. `todoke > out.txt`), and we must leave
/// that alone.
fn maybe_rebind(path: &str, which: STD_HANDLE, write: bool) {
    // SAFETY: Win32 API call.
    let existing = unsafe { GetStdHandle(which) };
    if !existing.is_null() && existing != INVALID_HANDLE_VALUE {
        // Redirection already in place — respect it.
        return;
    }

    let mut opts = OpenOptions::new();
    opts.read(true);
    if write {
        opts.write(true);
    }
    let Ok(file) = opts.open(path) else {
        return;
    };

    // Transfer ownership of the HANDLE out of `file` so its Drop won't
    // close what we just handed to SetStdHandle. If SetStdHandle
    // fails, we own an orphan HANDLE and must close it ourselves.
    let handle = file.into_raw_handle();
    // SAFETY: Win32 call; `handle` is a valid OS HANDLE we just took
    // ownership of via IntoRawHandle.
    let ok = unsafe { SetStdHandle(which, handle as _) };
    if ok == 0 {
        // SAFETY: `handle` was valid and is still owned by us.
        unsafe {
            CloseHandle(handle as _);
        }
    }
}
