//! OS-specific helpers: pipe path defaults, detached spawn, etc.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};

/// macOS: make `cmd` resolve and run with the user's login-shell `PATH`.
///
/// When todoke is started by LaunchServices (file association, Dock) it only
/// inherits launchd's minimal `PATH`, so a bare `neovide` can't be found and
/// anything the child spawns (mise / Homebrew tools) is missing too. We ask
/// `$SHELL -lc 'echo -n "$PATH"'` once per process and set it on the child.
/// Skipped when the inherited `PATH` is already richer than the system
/// default (terminal launch), so that path pays no login-shell startup cost.
/// Fail-soft: any failure leaves the inherited `PATH` untouched. Call before
/// applying user-configured `env` so an explicit `PATH` there still wins.
pub fn apply_login_path(cmd: &mut Command) {
    #[cfg(target_os = "macos")]
    {
        let inherited = std::env::var("PATH").unwrap_or_default();
        if is_minimal_path(&inherited)
            && let Some(resolved) = LOGIN_PATH.as_deref()
        {
            cmd.env("PATH", resolved);
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = cmd;
}

/// True when every `PATH` entry is part of the stock macOS system set.
#[cfg(any(target_os = "macos", test))]
fn is_minimal_path(path: &str) -> bool {
    const SYSTEM: [&str; 4] = ["/usr/bin", "/bin", "/usr/sbin", "/sbin"];
    path.split(':')
        .filter(|s| !s.is_empty())
        .all(|s| SYSTEM.contains(&s))
}

#[cfg(target_os = "macos")]
static LOGIN_PATH: std::sync::LazyLock<Option<String>> = std::sync::LazyLock::new(|| {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
    let output = Command::new(&shell)
        .args(["-lc", "echo -n \"$PATH\""])
        .output()
        .inspect_err(|e| tracing::warn!("login shell PATH: spawn {shell} failed: {e}"))
        .ok()?;
    if !output.status.success() {
        tracing::warn!("login shell PATH: {shell} exited with {}", output.status);
        return None;
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!path.is_empty()).then_some(path)
});

#[cfg(windows)]
use std::ffi::OsString;
#[cfg(windows)]
use std::process::Stdio;

/// Windows `CreateProcess` flag: child inherits no console.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// Windows `CreateProcess` flag: child gets no console at all.
#[cfg(windows)]
const DETACHED_PROCESS: u32 = 0x0000_0008;

/// Spawn a process detached so it survives the parent's exit.
///
/// When `gui` is `true`, the handler is assumed to be a GUI app (neovide,
/// nvim-qt, VSCode, firefox, …) — on Windows we skip the `cmd /c start`
/// wrapper and use `CREATE_NO_WINDOW + DETACHED_PROCESS` so no transient
/// cmd window flashes before the GUI appears. stdio is set to `Stdio::null`
/// since a GUI app doesn't need the parent terminal.
///
/// When `gui` is `false` (TUI / console handlers — nvim in a terminal,
/// helix, bat, …), we go through `cmd.exe /c start "" <program> <args>...`
/// rather than calling `CreateProcess(CREATE_NEW_CONSOLE)` directly. Reason:
/// Rust's `Command` defaults to inheriting parent stdio, which passes
/// explicit handles in `STARTUPINFO`. Those handles take precedence over the
/// new console the flag allocates, so a spawned TUI program would read from
/// the parent shell's stdin — the new console window appears but is dead to
/// keyboard input. `cmd.exe`'s `start` builtin goes through the Windows
/// shell APIs that allocate the new console AND set up its stdio correctly.
///
/// # Unix
///
/// `gui` is ignored; plain `spawn()` is used and the child inherits the
/// calling terminal. Desktop users who want a separate window should
/// configure a GUI editor (neovide, gvim, code, …) in their config — the
/// app itself handles window creation.
pub fn spawn_detached(cmd: &mut Command, gui: bool, _file_for_log: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        if gui {
            return spawn_detached_gui_windows(cmd);
        }
        spawn_detached_console_windows(cmd)
    }

    #[cfg(not(windows))]
    {
        let _ = gui;
        cmd.spawn().context("spawn failed")?;
        Ok(())
    }
}

#[cfg(windows)]
fn spawn_detached_gui_windows(cmd: &mut Command) -> Result<()> {
    use std::os::windows::process::CommandExt;

    // `creation_flags` overwrites any existing flags wholesale (std doesn't
    // expose a getter to merge with). Callers inside this crate don't set
    // creation flags before calling spawn_detached, so this is safe today.
    // If a future caller needs to preserve other flags, swap this for the
    // CreateProcessW path directly and OR them in explicitly.
    cmd.creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    cmd.spawn()
        .context("spawn detached GUI (CREATE_NO_WINDOW) failed")?;
    Ok(())
}

/// TUI / console path. Unlike the GUI path above this does NOT spawn `cmd`
/// directly; it copies its program / args / cwd / envs onto a fresh
/// `cmd.exe /c start ""` wrapper and spawns that instead, because `start`
/// is the only builtin that allocates a new console with correctly-wired
/// stdio on Windows. The original `cmd` is read-only here — callers that
/// care about `cmd` state afterwards shouldn't rely on it being mutated.
#[cfg(windows)]
fn spawn_detached_console_windows(cmd: &mut Command) -> Result<()> {
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

#[cfg(test)]
mod login_path_tests {
    use super::is_minimal_path;

    #[test]
    fn minimal_path_detection() {
        assert!(is_minimal_path("/usr/bin:/bin:/usr/sbin:/sbin"));
        assert!(is_minimal_path(""));
        assert!(!is_minimal_path("/usr/bin:/opt/homebrew/bin"));
    }
}
