//! Neovim backend.
//!
//! Dispatch matrix:
//!
//! | mode    | sync  | behavior                                                     |
//! |---------|-------|--------------------------------------------------------------|
//! | remote  | false | connect to listen pipe; on fail, exec/spawn nvim with --listen |
//! | new     | false | always spawn detached (no --listen)                           |
//! | new     | true  | spawn as child, wait for exit, propagate exit code            |
//! | remote  | true  | falls back to `new + true` with a warning (v0.2 TODO)         |
//! | <other> | *     | treated as `remote` (RPC with `args.<mode>` list ignored)     |
//!
//! Input type: only file inputs are supported; URL inputs are rejected with
//! a warn and skipped at the caller level.

use std::path::{Path, PathBuf};
use std::process::{Command as StdCommand, Stdio};

use anyhow::{Context as _, Result, anyhow};
use nvim_rs::{Handler, compat::tokio::Compat, create::tokio as create};
use tokio::io::WriteHalf;
use tracing::{debug, info, warn};

use crate::matcher::vim_path as vim_path_fn;
use crate::platform;

#[cfg(unix)]
type NvimConnection = tokio::net::UnixStream;
#[cfg(windows)]
type NvimConnection = tokio::net::windows::named_pipe::NamedPipeClient;

type NvimWriter = Compat<WriteHalf<NvimConnection>>;

#[derive(Clone)]
struct DummyHandler;

impl Handler for DummyHandler {
    type Writer = NvimWriter;
}

/// A neovim dispatch. Uses well-known mode names `"remote"` and `"new"`;
/// anything else is treated as `"remote"` with a warning. `args_remote` is
/// injected between files and `--listen`; `args_new` goes before files when
/// spawning fresh (no --listen).
#[derive(Debug, Clone)]
pub struct NeovimBackend {
    pub command: String,
    pub listen: String,
    pub args_remote: Vec<String>,
    pub args_new: Vec<String>,
    /// Raw flag-like argv (e.g. `+42`, `-c :set ft=gitcommit`) forwarded to
    /// nvim's start-up command line. Inserted before the file list so the
    /// classic `nvim +42 file.txt` layout is honored.
    ///
    /// remote-mode dispatches can't forward these to an already-running
    /// nvim (the RPC session is long past start-up), so `dispatch_remote`
    /// warns and drops them. Spawn paths (`new`, or remote fallback
    /// `start_with_listen`) honor them.
    pub passthrough: Vec<String>,
    /// Flagged by the caller when the `command` is a GUI front-end
    /// (neovide, nvim-qt). Controls the detached-spawn code path on Windows.
    pub gui: bool,
    /// Set to `true` only when this dispatch is the last batch in the plan.
    /// `exec(2)` replaces the entire process — if more batches follow, they
    /// would never run. The dispatcher threads this flag so exec() is only
    /// used when it is safe (single-batch or last-batch scenarios).
    pub can_exec: bool,
}

impl NeovimBackend {
    pub async fn dispatch(&self, files: &[PathBuf], mode: &str, sync: bool) -> Result<()> {
        match (mode, sync) {
            ("remote", false) => self.dispatch_remote(files).await,
            ("new", false) => self.spawn_detached_fresh(files),
            ("new", true) => self.spawn_sync(files),
            ("remote", true) => {
                warn!("neovim remote+sync is not implemented yet; falling back to new+sync");
                self.spawn_sync(files)
            }
            (other, s) => {
                warn!(
                    mode = other,
                    "unknown mode for neovim backend; treating as remote"
                );
                if s {
                    self.spawn_sync(files)
                } else {
                    self.dispatch_remote(files).await
                }
            }
        }
    }

    /// Try to connect to the listen pipe; on success, send `:edit <file>` for
    /// each file (or `:enew` if the user invoked todoke with no files). On
    /// connect failure, spawn a detached nvim with `--listen` so the next
    /// remote dispatch finds it.
    async fn dispatch_remote(&self, files: &[PathBuf]) -> Result<()> {
        if !self.passthrough.is_empty() {
            warn!(
                passthrough = ?self.passthrough,
                "passthrough flags are ignored for remote-mode neovim (RPC is post-startup)",
            );
        }
        match create::new_path(self.listen.as_str(), DummyHandler).await {
            Ok((nvim, _io_handle)) => {
                info!(pipe = %self.listen, count = files.len(), "connected to existing nvim");
                if files.is_empty() {
                    debug!(cmd = "enew", "sending RPC");
                    nvim.command("enew").await.context("failed to send :enew")?;
                } else {
                    for f in files {
                        let vim_cmd = format!("edit {}", vim_path(f));
                        debug!(cmd = %vim_cmd, "sending RPC");
                        nvim.command(&vim_cmd)
                            .await
                            .with_context(|| format!("failed to send :{vim_cmd}"))?;
                    }
                }
                Ok(())
            }
            Err(e) => {
                info!(pipe = %self.listen, reason = %e, "no listener; starting nvim with --listen");
                self.start_with_listen(files)
            }
        }
    }

    /// Argv layout: `command <passthrough>... FILES... <args_remote>... --listen LISTEN`.
    ///
    /// On Unix with a non-GUI command, this replaces the current process via
    /// `exec` so nvim inherits the terminal as a foreground process (no SIGTTOU).
    /// On Windows or GUI targets, falls back to a detached spawn.
    fn start_with_listen(&self, files: &[PathBuf]) -> Result<()> {
        let mut cmd = StdCommand::new(&self.command);
        for p in &self.passthrough {
            cmd.arg(p);
        }
        for f in files {
            cmd.arg(f);
        }
        for a in &self.args_remote {
            cmd.arg(a);
        }
        cmd.arg("--listen").arg(&self.listen);

        #[cfg(unix)]
        if !self.gui && self.can_exec {
            use std::os::unix::process::CommandExt;
            // exec() replaces this process with nvim. The tokio runtime is
            // abandoned — safe only when this is the last (or only) batch in
            // the plan. The dispatcher sets can_exec = true only then, so
            // multi-batch invocations fall through to spawn_detached instead.
            // nvim inherits the calling terminal as a foreground process,
            // exactly like hitori.vim's singleton behaviour.
            let err = cmd.exec();
            return Err(
                anyhow::Error::from(err).context(format!("failed to exec {}", self.command))
            );
        }

        platform::spawn_detached(
            &mut cmd,
            self.gui,
            files.first().map(PathBuf::as_path).unwrap_or(Path::new("")),
        )
        .with_context(|| format!("failed to spawn {}", self.command))?;
        Ok(())
    }

    fn spawn_detached_fresh(&self, files: &[PathBuf]) -> Result<()> {
        let mut cmd = StdCommand::new(&self.command);
        for a in &self.args_new {
            cmd.arg(a);
        }
        for p in &self.passthrough {
            cmd.arg(p);
        }
        for f in files {
            cmd.arg(f);
        }
        platform::spawn_detached(
            &mut cmd,
            self.gui,
            files.first().map(PathBuf::as_path).unwrap_or(Path::new("")),
        )
        .with_context(|| format!("failed to spawn {}", self.command))?;
        Ok(())
    }

    fn spawn_sync(&self, files: &[PathBuf]) -> Result<()> {
        let mut cmd = StdCommand::new(&self.command);
        for a in &self.args_new {
            cmd.arg(a);
        }
        for p in &self.passthrough {
            cmd.arg(p);
        }
        for f in files {
            cmd.arg(f);
        }
        // inherit stdio so nvim can draw to the parent terminal (this is the
        // $EDITOR=todoke use case: git invokes todoke with a TTY attached,
        // and nvim must take over that TTY).
        cmd.stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());

        let status = cmd
            .status()
            .with_context(|| format!("failed to run {}", self.command))?;

        if status.success() {
            Ok(())
        } else {
            Err(anyhow!("{} exited with status {}", self.command, status))
        }
    }
}

/// Path form that nvim `:edit` reliably accepts across platforms.
/// Delegates to [`matcher::vim_path`] which keeps UNC backslashes intact.
fn vim_path(p: &Path) -> String {
    vim_path_fn(p)
}
