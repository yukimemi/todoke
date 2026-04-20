//! Generic backend: spawn a CLI editor with template-expanded arguments.
//!
//! File placement semantics (v0.1):
//! - Arg templates are rendered ONCE using the FIRST file's context.
//! - All files are then appended as trailing positional arguments.
//!
//! This matches 99% of editors (VSCode `--reuse-window`, vim, emacs, etc.) and
//! keeps the mental model simple. Per-file arg placement is deferred to v0.2.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command as StdCommand, Stdio};

use anyhow::{Context as _, Result, anyhow};
use tracing::{debug, info};

use crate::config::Mode;
use crate::platform;
use crate::template::{FileParts, build_context, render};

#[derive(Debug, Clone)]
pub struct GenericBackend {
    pub command: String,
    pub args_new: Vec<String>,
    pub args_remote: Vec<String>,
    pub env: BTreeMap<String, String>,
}

pub struct DispatchCtx<'a> {
    pub files: &'a [PathBuf],
    pub mode: Mode,
    pub sync: bool,
    pub group: &'a str,
    pub rule_name: &'a str,
    pub vars: &'a BTreeMap<String, toml::Value>,
    pub cwd: &'a str,
}

impl GenericBackend {
    pub fn dispatch(&self, dctx: DispatchCtx<'_>) -> Result<()> {
        if dctx.files.is_empty() {
            return Ok(());
        }

        let args_template = match dctx.mode {
            Mode::Remote => &self.args_remote,
            Mode::New => &self.args_new,
        };

        let rendered_args = self.render_args(args_template, &dctx)?;

        let mut cmd = StdCommand::new(&self.command);
        cmd.args(&rendered_args);
        for f in dctx.files {
            cmd.arg(f);
        }
        for (k, v) in &self.env {
            cmd.env(k, v);
        }

        debug!(
            command = %self.command,
            args = ?rendered_args,
            files = ?dctx.files,
            sync = dctx.sync,
            "generic dispatch"
        );

        if dctx.sync {
            self.run_sync(&mut cmd)
        } else {
            self.run_detached(&mut cmd, dctx.files.first().map(PathBuf::as_path))
        }
    }

    fn render_args(&self, templates: &[String], dctx: &DispatchCtx<'_>) -> Result<Vec<String>> {
        let first = FileParts::from_path(dctx.files.first().unwrap());
        let editor_parts = FileParts::from_path(Path::new(&self.command));
        let ctx = build_context(
            &first,
            Some(&editor_parts),
            dctx.cwd,
            dctx.group,
            dctx.rule_name,
            dctx.vars,
        );
        let mut tera = crate::template::new_engine();
        templates
            .iter()
            .map(|t| {
                render(&mut tera, t, &ctx).with_context(|| format!("rendering arg template: {t}"))
            })
            .collect()
    }

    fn run_detached(&self, cmd: &mut StdCommand, file_for_log: Option<&Path>) -> Result<()> {
        info!(command = %self.command, "spawning detached");
        platform::spawn_detached(cmd, file_for_log.unwrap_or(Path::new("")))
            .with_context(|| format!("failed to spawn {}", self.command))
    }

    fn run_sync(&self, cmd: &mut StdCommand) -> Result<()> {
        info!(command = %self.command, "spawning sync (inherit stdio)");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_template_args_with_file_context() {
        let backend = GenericBackend {
            command: "echo".into(),
            args_new: vec![
                "--file={{ file_stem }}".into(),
                "--ext={{ file_ext }}".into(),
            ],
            args_remote: vec![],
            env: BTreeMap::new(),
        };
        let files = vec![PathBuf::from("/tmp/hello.rs")];
        let vars = BTreeMap::new();
        let dctx = DispatchCtx {
            files: &files,
            mode: Mode::New,
            sync: false,
            group: "g",
            rule_name: "r",
            vars: &vars,
            cwd: "/tmp",
        };
        let args = backend.render_args(&backend.args_new, &dctx).unwrap();
        assert_eq!(args, vec!["--file=hello", "--ext=rs"]);
    }
}
