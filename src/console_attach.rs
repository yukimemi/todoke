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
//! 2. Re-open `CONOUT$` / `CONIN$` and `SetStdHandle` them so stdio
//!    actually points somewhere Rust's `println!` / reads can use.
//!
//! When launched from explorer `AttachConsole` returns 0 and this whole
//! function is a no-op (stdio stays null — exactly what we want).

use std::fs::OpenOptions;
use std::os::windows::io::AsRawHandle;

use windows_sys::Win32::System::Console::{
    ATTACH_PARENT_PROCESS, AttachConsole, STD_ERROR_HANDLE, STD_HANDLE, STD_INPUT_HANDLE,
    STD_OUTPUT_HANDLE, SetStdHandle,
};

pub fn attach_parent_console() {
    // SAFETY: Win32 API call; the return value distinguishes success (1)
    // from no-parent-console (0).
    if unsafe { AttachConsole(ATTACH_PARENT_PROCESS) } == 0 {
        return;
    }
    rebind("CONOUT$", STD_OUTPUT_HANDLE, true);
    rebind("CONOUT$", STD_ERROR_HANDLE, true);
    rebind("CONIN$", STD_INPUT_HANDLE, false);
}

/// Open the console device and wire it to one of the standard handles.
/// The opened `File` is deliberately `mem::forget`-ed: dropping it would
/// close the HANDLE we just gave to `SetStdHandle`.
fn rebind(path: &str, which: STD_HANDLE, write: bool) {
    let mut opts = OpenOptions::new();
    opts.read(true);
    if write {
        opts.write(true);
    }
    let Ok(file) = opts.open(path) else {
        return;
    };
    let handle = file.as_raw_handle();
    // SAFETY: Win32 call, handle is a valid OS handle obtained above.
    unsafe {
        SetStdHandle(which, handle as _);
    }
    std::mem::forget(file);
}
