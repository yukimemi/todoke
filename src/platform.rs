//! OS-specific helpers: pipe path defaults, detached spawn, etc.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};

#[cfg(windows)]
use std::ffi::OsString;

/// Spawn a process detached so it survives the parent's exit.
///
/// # Windows
///
/// We go through `cmd.exe /c start "" <program> <args>...` rather than
/// `CreateProcess(CREATE_NEW_CONSOLE)` directly. Reason: Rust's `Command`
/// defaults to inheriting parent stdio, which passes explicit handles in
/// `STARTUPINFO`. Those handles take precedence over the new console the flag
/// allocates, so a spawned TUI program (nvim, vim, helix, …) would read from
/// the parent shell's stdin — the new console window appears but is dead to
/// keyboard input. `cmd.exe`'s `start` builtin goes through the Windows shell
/// APIs that allocate the new console AND set up its stdio correctly.
///
/// # Unix
///
/// Plain `spawn()` — the child inherits the calling terminal. Desktop users
/// who want a separate window should configure a GUI editor (neovide, gvim,
/// code, …) in their config.
pub fn spawn_detached(cmd: &mut Command, _file_for_log: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        let program = cmd.get_program().to_os_string();
        let args: Vec<OsString> = cmd.get_args().map(|s| s.to_os_string()).collect();
        let cwd = cmd.get_current_dir().map(|p| p.to_path_buf());
        let envs: Vec<(OsString, Option<OsString>)> = cmd
            .get_envs()
            .map(|(k, v)| (k.to_os_string(), v.map(|s| s.to_os_string())))
            .collect();

        let mut wrapper = Command::new("cmd");
        // `/c start "" program args...`
        // The empty "" is start's window-title slot. Without it, start would
        // consume the first quoted argument as a title when the program path
        // contains spaces.
        wrapper.arg("/c").arg("start").arg("");
        wrapper.arg(&program);
        for a in &args {
            wrapper.arg(a);
        }
        if let Some(d) = cwd {
            wrapper.current_dir(d);
        }
        for (k, v) in envs {
            match v {
                Some(v) => {
                    wrapper.env(&k, v);
                }
                None => {
                    wrapper.env_remove(&k);
                }
            }
        }
        wrapper.spawn().context("spawn via cmd /c start failed")?;
        Ok(())
    }

    #[cfg(not(windows))]
    {
        cmd.spawn().context("spawn failed")?;
        Ok(())
    }
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
