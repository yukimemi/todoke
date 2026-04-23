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
use std::sync::OnceLock;

use anyhow::{Context as _, Result, anyhow};
use regex::Regex;
use tracing::{debug, info};

use crate::input::Input;
use crate::matcher::CaptureMap;
use crate::platform;
use crate::template::{Context, build_context, render};

/// True when the joined args text references `{{ input }}` or any of the
/// input-derived context vars (`file_*`, `url_*`). `cap.*` is intentionally
/// excluded — captures are ambiguous enough that their presence can't be
/// taken as proof of input reconstruction.
fn references_input(text: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re =
        RE.get_or_init(|| Regex::new(r"\{\{\s*(input|file_\w+|url_\w+)\b").expect("static regex"));
    re.is_match(text)
}

/// True when the joined args text references `{{ passthrough }}` in any
/// form (standalone, filtered, iterated).
fn references_passthrough(text: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"\{\{\s*passthrough\b|\bfor\s+\w+\s+in\s+passthrough\b").expect("static regex")
    });
    re.is_match(text)
}

#[derive(Debug, Clone)]
pub struct ExecBackend {
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    /// `None` → auto (append when no args template references input/
    /// file_*/url_*); `Some(true)` → always append; `Some(false)` → never.
    pub append_inputs: Option<bool>,
    /// Same semantics as `append_inputs` but keyed on `{{ passthrough }}`
    /// references.
    pub append_passthrough: Option<bool>,
    /// GUI handler hint — controls the detached-spawn code path on Windows
    /// (skip `cmd /c start` wrapper when true so no cmd window flashes).
    pub gui: bool,
}

pub struct DispatchCtx<'a> {
    pub inputs: &'a [Input],
    /// Raw argv strings from `passthrough = true` rules. Inserted after the
    /// rendered `args` list and before the trailing input append, so target
    /// command lines look like `cmd <args> <passthrough> <inputs>`.
    pub passthrough: &'a [String],
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

        // Auto-detect template references so "use {{ input }} in args"
        // doesn't end up also pasting the raw value at the end. Scan the
        // raw template strings (pre-render) so a reference inside a
        // `{% if %}` branch still counts — false positives are preferable
        // to double-insertion here.
        let args_joined = self.args.join("\n");
        let append_inputs = self
            .append_inputs
            .unwrap_or_else(|| !references_input(&args_joined));
        let append_passthrough = self
            .append_passthrough
            .unwrap_or_else(|| !references_passthrough(&args_joined));

        let mut cmd = StdCommand::new(&self.command);
        cmd.args(&rendered_args);
        if append_passthrough {
            for p in dctx.passthrough {
                cmd.arg(p);
            }
        }
        if append_inputs {
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
            passthrough = ?dctx.passthrough,
            count = dctx.inputs.len(),
            append_inputs,
            append_passthrough,
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
            passthrough: dctx.passthrough,
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
        platform::spawn_detached(cmd, self.gui, Path::new(""))
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
    fn references_input_detects_input_and_derived_vars() {
        assert!(references_input("{{ input }}"));
        assert!(references_input("prefix {{  input  }} suffix"));
        assert!(references_input("--file={{ file_path }}"));
        assert!(references_input("--stem={{ file_stem }}"));
        assert!(references_input("--host={{ url_host }}"));
        assert!(references_input("--query={{ url_query }}"));
        // cap is intentionally excluded.
        assert!(!references_input("{{ cap.1 }}"));
        assert!(!references_input("{{ cap.name }}"));
        // Unrelated vars.
        assert!(!references_input("{{ group }}"));
        assert!(!references_input("{{ rule }}"));
        assert!(!references_input("{{ command_name }}"));
        // Empty / literal text.
        assert!(!references_input(""));
        assert!(!references_input("--flag"));
        // Filter form.
        assert!(references_input("{{ input | upper }}"));
    }

    #[test]
    fn references_passthrough_detects_various_forms() {
        assert!(references_passthrough("{{ passthrough }}"));
        assert!(references_passthrough("{{ passthrough | join(sep=' ') }}"));
        assert!(references_passthrough(
            "{% for p in passthrough %}{{ p }}{% endfor %}"
        ));
        assert!(!references_passthrough("{{ input }}"));
        assert!(!references_passthrough("{{ cap.1 }}"));
        assert!(!references_passthrough(""));
    }

    #[test]
    fn renders_template_args_with_file_context() {
        let backend = ExecBackend {
            command: "echo".into(),
            args: vec![
                "--file={{ file_stem }}".into(),
                "--ext={{ file_ext }}".into(),
            ],
            env: BTreeMap::new(),
            append_inputs: Some(true),
            append_passthrough: None,
            gui: false,
        };
        let inputs = vec![Input::File(PathBuf::from("/tmp/hello.rs"))];
        let passthrough: Vec<String> = Vec::new();
        let vars = BTreeMap::new();
        let cap = CaptureMap::new();
        let dctx = DispatchCtx {
            inputs: &inputs,
            passthrough: &passthrough,
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
