//! Neovim backend.
//!
//! Dispatch matrix (for v0.1):
//!
//! | mode    | sync  | behavior                                                     |
//! |---------|-------|--------------------------------------------------------------|
//! | remote  | false | connect to listen pipe; on fail, spawn detached with --listen |
//! | new     | false | always spawn detached (no --listen)                           |
//! | new     | true  | spawn as child, wait for exit, propagate exit code            |
//! | remote  | true  | v0.1 falls back to `new + true` with a warning                |
//!
//! remote+sync via `nvim_buf_attach` is queued for v0.2.

use std::path::{Path, PathBuf};
use std::process::{Command as StdCommand, Stdio};

use anyhow::{Context as _, Result, anyhow};
use nvim_rs::{Handler, compat::tokio::Compat, create::tokio as create};
use tokio::io::WriteHalf;
use tracing::{debug, info, warn};

use crate::config::Mode;
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

#[derive(Debug, Clone)]
pub struct NeovimBackend {
    pub command: String,
    pub listen: String,
}

impl NeovimBackend {
    pub async fn dispatch(&self, files: &[PathBuf], mode: Mode, sync: bool) -> Result<()> {
        if files.is_empty() {
            return Ok(());
        }

        match (mode, sync) {
            (Mode::Remote, false) => self.dispatch_remote(files).await,
            (Mode::New, false) => self.spawn_detached_fresh(files),
            (Mode::New, true) => self.spawn_sync(files),
            (Mode::Remote, true) => {
                warn!("neovim remote+sync is not implemented yet (v0.2); falling back to new+sync");
                self.spawn_sync(files)
            }
        }
    }

    /// Try to connect to the listen pipe; on success, send `:edit <file>` for
    /// each file. On connect failure, spawn a detached nvim with `--listen` so
    /// the next remote dispatch finds it.
    async fn dispatch_remote(&self, files: &[PathBuf]) -> Result<()> {
        match create::new_path(self.listen.as_str(), DummyHandler).await {
            Ok((nvim, _io_handle)) => {
                info!(pipe = %self.listen, count = files.len(), "connected to existing nvim");
                for f in files {
                    let vim_cmd = format!("edit {}", vim_path(f));
                    debug!(cmd = %vim_cmd, "sending RPC");
                    nvim.command(&vim_cmd)
                        .await
                        .with_context(|| format!("failed to send :{vim_cmd}"))?;
                }
                Ok(())
            }
            Err(e) => {
                info!(pipe = %self.listen, reason = %e, "no listener; spawning detached nvim");
                self.spawn_detached_with_listen(files)
            }
        }
    }

    fn spawn_detached_with_listen(&self, files: &[PathBuf]) -> Result<()> {
        let mut cmd = StdCommand::new(&self.command);
        cmd.arg("--listen").arg(&self.listen);
        for f in files {
            cmd.arg(f);
        }
        platform::spawn_detached(
            &mut cmd,
            files.first().map(PathBuf::as_path).unwrap_or(Path::new("")),
        )
        .with_context(|| format!("failed to spawn {}", self.command))?;
        Ok(())
    }

    fn spawn_detached_fresh(&self, files: &[PathBuf]) -> Result<()> {
        let mut cmd = StdCommand::new(&self.command);
        for f in files {
            cmd.arg(f);
        }
        platform::spawn_detached(
            &mut cmd,
            files.first().map(PathBuf::as_path).unwrap_or(Path::new("")),
        )
        .with_context(|| format!("failed to spawn {}", self.command))?;
        Ok(())
    }

    fn spawn_sync(&self, files: &[PathBuf]) -> Result<()> {
        let mut cmd = StdCommand::new(&self.command);
        for f in files {
            cmd.arg(f);
        }
        // inherit stdio so nvim can draw to the parent terminal (this is the
        // $EDITOR=edtr use case: git invokes edtr with a TTY attached, and
        // nvim must take over that TTY).
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
