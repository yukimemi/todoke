//! Exec backend: spawn a command with template-expanded arguments, for any
//! input — files, URLs, whatever. Uses `Target.args.<mode>` with
//! `args.default` as the fallback list.
//!
//! File placement semantics:
//! - Arg templates are rendered ONCE using the FIRST input's context.
//! - All inputs are then appended as trailing positional arguments (their
//!   `display_string`, which is the path for files or the URL string).
//!
//! This matches 99% of apps (VSCode `--reuse-window`, vim, emacs, firefox,
//! …) and keeps the mental model simple.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command as StdCommand, Stdio};

use anyhow::{Context as _, Result, anyhow};
use tracing::{debug, info};

use crate::input::Input;
use crate::matcher::CaptureMap;
use crate::platform;
use crate::template::{Context, build_context, render};

#[derive(Debug, Clone)]
pub struct ExecBackend {
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    /// Whether to append each input's display string as a trailing arg
    /// after the rendered `args` list. Defaults to true.
    pub append_inputs: bool,
}

pub struct DispatchCtx<'a> {
    pub inputs: &'a [Input],
    pub mode: &'a str,
    pub sync: bool,
    pub group: &'a str,
    pub rule_name: &'a str,
    pub vars: &'a BTreeMap<String, toml::Value>,
    pub cwd: &'a str,
    pub cap: &'a CaptureMap,
}

impl ExecBackend {
    pub fn dispatch(&self, dctx: DispatchCtx<'_>) -> Result<()> {
        let rendered_args = self.render_args(&dctx)?;

        let mut cmd = StdCommand::new(&self.command);
        cmd.args(&rendered_args);
        if self.append_inputs {
            for i in dctx.inputs {
                cmd.arg(i.display_string());
            }
        }
        for (k, v) in &self.env {
            cmd.env(k, v);
        }

        debug!(
            command = %self.command,
            args = ?rendered_args,
            count = dctx.inputs.len(),
            append_inputs = self.append_inputs,
            sync = dctx.sync,
            mode = dctx.mode,
            "exec dispatch"
        );

        if dctx.sync {
            self.run_sync(&mut cmd)
        } else {
            self.run_detached(&mut cmd)
        }
    }

    fn render_args(&self, dctx: &DispatchCtx<'_>) -> Result<Vec<String>> {
        let ctx = build_context(Context {
            input: dctx.inputs.first(),
            command: &self.command,
            cwd: dctx.cwd,
            group: dctx.group,
            rule_name: dctx.rule_name,
            vars: dctx.vars,
            cap: dctx.cap,
        });
        let mut tera = crate::template::new_engine();
        self.args
            .iter()
            .map(|t| {
                render(&mut tera, t, &ctx).with_context(|| format!("rendering arg template: {t}"))
            })
            .collect()
    }

    fn run_detached(&self, cmd: &mut StdCommand) -> Result<()> {
        info!(command = %self.command, "spawning detached");
        platform::spawn_detached(cmd, Path::new(""))
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
    use std::path::PathBuf;

    #[test]
    fn renders_template_args_with_file_context() {
        let backend = ExecBackend {
            command: "echo".into(),
            args: vec![
                "--file={{ file_stem }}".into(),
                "--ext={{ file_ext }}".into(),
            ],
            env: BTreeMap::new(),
            append_inputs: true,
        };
        let inputs = vec![Input::File(PathBuf::from("/tmp/hello.rs"))];
        let vars = BTreeMap::new();
        let cap = CaptureMap::new();
        let dctx = DispatchCtx {
            inputs: &inputs,
            mode: "new",
            sync: false,
            group: "g",
            rule_name: "r",
            vars: &vars,
            cwd: "/tmp",
            cap: &cap,
        };
        let args = backend.render_args(&dctx).unwrap();
        assert_eq!(args, vec!["--file=hello", "--ext=rs"]);
    }
}
