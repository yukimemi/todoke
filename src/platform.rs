//! OS-specific helpers: pipe path defaults, detached spawn, etc.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
pub const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;

/// Spawn a process detached so it survives the parent's exit.
/// On Windows this opens a new console (needed when edtr is invoked as OS
/// default program with no attached terminal).
pub fn spawn_detached(cmd: &mut Command, _file_for_log: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        cmd.creation_flags(CREATE_NEW_CONSOLE);
    }
    cmd.spawn().context("spawn failed")?;
    Ok(())
}

pub fn is_windows() -> bool {
    cfg!(target_os = "windows")
}

pub fn is_linux() -> bool {
    cfg!(target_os = "linux")
}

pub fn is_mac() -> bool {
    cfg!(target_os = "macos")
}
