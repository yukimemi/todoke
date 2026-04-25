//! TOML + Tera config schema.
//!
//! Two layers:
//! - [`Config`]: the raw TOML deserialization target.
//! - [`ResolvedConfig`]: [`Config`] + pre-compiled regex patterns + validated
//!   cross-references. Everything you actually want to use at dispatch time.
//!
//! Tera expansion happens at dispatch time (not load time) because rule.group
//! and todoke.* templates can reference per-input context.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, anyhow};
use directories::BaseDirs;
use regex::Regex;
use serde::Deserialize;

use crate::input::InputKind;

pub const DEFAULT_CONFIG_TOML: &str = include_str!("../assets/default.toml");

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub vars: BTreeMap<String, toml::Value>,
    /// Named targets for delivery. Keyed by handler name, referenced from
    /// `rule.to`.
    #[serde(default)]
    pub todoke: BTreeMap<String, Target>,
    #[serde(default)]
    pub rules: Vec<Rule>,
}

/// A named delivery target. Describes what happens when a rule picks this
/// entry: a command to spawn, optional per-mode arg lists, and optional
/// neovim-specific fields when `kind = "neovim"`.
#[derive(Debug, Clone, Deserialize)]
pub struct Target {
    /// `"exec"` (default) spawns `command` with the resolved args.
    /// `"neovim"` enables msgpack-RPC reuse of a running nvim on `listen`.
    #[serde(default)]
    pub kind: TargetKind,
    pub command: String,
    #[serde(default)]
    pub listen: Option<String>,
    /// Per-mode arg lists. `args.default` (if present) is the fallback when
    /// the rule's `mode` has no matching key in this map.
    #[serde(default)]
    pub args: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Controls whether the exec backend appends each input's display
    /// string as a trailing positional arg after the rendered `args` list.
    ///
    /// - `None` / omitted (**auto**, the default): appended **unless** any
    ///   `args` template references `{{ input }}` / `{{ file_* }}` /
    ///   `{{ url_* }}` — in which case the trailing append is skipped so
    ///   the same value isn't passed twice. `{{ cap.* }}` is **not** a
    ///   signal (cap can be used for extraction unrelated to input
    ///   reconstruction).
    /// - `Some(true)`: force append regardless of templates.
    /// - `Some(false)`: force skip regardless of templates.
    #[serde(default)]
    pub append_inputs: Option<bool>,
    /// Controls whether passthrough-rule argv (`+42`, `-c :set …`, …) is
    /// appended after the rendered `args` list. Same auto / true / false
    /// semantics as [`Self::append_inputs`], but the auto trigger is
    /// a `{{ passthrough }}` reference (any form).
    #[serde(default)]
    pub append_passthrough: Option<bool>,
    /// Set to `true` when the handler is a **GUI** application (neovide,
    /// nvim-qt, vscode, firefox, …). On Windows, detached spawns then use
    /// `CREATE_NO_WINDOW + DETACHED_PROCESS` instead of the default
    /// `cmd /c start` wrapper, so no transient cmd window flashes before
    /// the GUI appears. On Unix this flag is a no-op.
    ///
    /// Leave unset / `false` for console / TUI handlers (nvim in a terminal,
    /// helix, bat, …) — those rely on the fresh console window that
    /// `cmd /c start` allocates.
    #[serde(default)]
    pub gui: bool,
}

impl Target {
    /// Look up the arg list for a given mode, falling back to `args.default`
    /// and then to an empty list.
    pub fn args_for(&self, mode: &str) -> &[String] {
        self.args
            .get(mode)
            .or_else(|| self.args.get("default"))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TargetKind {
    #[default]
    Exec,
    Neovim,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Rule {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(rename = "match")]
    pub match_: StringOrVec,
    /// Negative filter. When any `exclude` pattern hits the input, this rule
    /// does NOT apply even if `match` hits — todoke keeps looking at
    /// subsequent rules. Accepts a single pattern or an array.
    #[serde(default)]
    pub exclude: Option<StringOrVec>,
    /// Name of a `[todoke.<name>]` entry to deliver the matched input to.
    /// Tera-templated — `to = "{{ vars.gui }}"` works.
    ///
    /// **Optional only for `passthrough = true` rules.** A passthrough
    /// rule with no `to` acts as "collect argv, let another rule decide
    /// the target": at Phase 2b, the passthrough is merged into the
    /// already-built batch that shares its resolved `group` (target
    /// isn't required to match). If no such batch exists, the
    /// passthrough is dropped with a warning. Useful for generic flag
    /// rules like `match = '^[-+]'` that should ride along with whoever
    /// the other rules decided to deliver to.
    ///
    /// Normal (non-passthrough) rules and joined rules still require `to`.
    #[serde(default)]
    pub to: Option<String>,
    #[serde(default)]
    pub group: Option<String>,
    /// Free-form mode string. For `kind = "neovim"` the reserved values
    /// `"remote"` and `"new"` select RPC reuse vs fresh spawn. For
    /// `kind = "exec"` the value is used purely to pick the matching
    /// `target.args.<mode>` list.
    #[serde(default = "default_mode")]
    pub mode: String,
    #[serde(default)]
    pub sync: bool,
    /// Restrict which [`crate::input::InputKind`]s this rule can match.
    /// Accepts a single kind (`"file"`) or an array (`["file", "raw"]`).
    /// Omitted = no restriction (all kinds allowed).
    ///
    /// Needed because auto-detection treats bare words like `HEAD` / `main`
    /// as files — a rule that wants to handle those as git refs should set
    /// `input_type = "raw"` so it only fires for `--as raw HEAD`.
    #[serde(default)]
    pub input_type: Option<InputTypes>,
    /// When true, this rule matches against the space-joined argv (all
    /// inputs concatenated) instead of each input individually. On a hit,
    /// the named capture `input` is re-classified via `Input::from_arg`
    /// and becomes the sole input of the resulting batch; the remaining
    /// captures are available to the target's arg templates as
    /// `{{ cap.<name> }}`.
    ///
    /// Designed for the `$EDITOR=todoke +42 file.txt` pattern where the
    /// caller passes editor-specific flags ahead of the file. Mutually
    /// exclusive with `passthrough`.
    #[serde(default)]
    pub joined: bool,
    /// When true, inputs matched by this rule are NOT opened (no `:edit`,
    /// no URL open, no positional append). Instead, the raw argv string is
    /// injected into the target's start-up argv as a passthrough flag.
    /// Use for rules like `match = '^[-+]'` that catch editor flags
    /// (`+42`, `-c :set ft=...`) and forward them verbatim to the handler
    /// command line.
    ///
    /// Mutually exclusive with `joined` (joined achieves the same effect
    /// via capture-driven arg templates).
    #[serde(default)]
    pub passthrough: bool,
    /// Number of following argv items to **also** forward as passthrough
    /// when this rule matches. Only meaningful when `passthrough = true`.
    ///
    /// Designed for spaced-value editor flags like `-c :set ft=md` where
    /// the value (`:set ft=md`) is its own argv. With `consumes = 1`,
    /// matching `^-c$` on the flag pulls the next argv along so both
    /// strings reach the target's start-up command line intact.
    #[serde(default)]
    pub consumes: usize,
    /// Regex. When set, `-p a.txt b.txt c.txt` style multi-value flags
    /// can be absorbed wholesale: the passthrough rule matches `-p`,
    /// then todoke keeps eating argv until one matches this regex
    /// (or argv ends). The stopper argv itself is NOT consumed.
    ///
    /// Typical values: `'^[-+]'` (stop at next flag), `'^--$'` (stop at
    /// GNU separator). Mutually exclusive with `consumes` and
    /// `consumes_rest`; only valid when `passthrough = true`.
    #[serde(default)]
    pub consumes_until: Option<String>,
    /// Consume every remaining argv as part of this passthrough. Useful
    /// for "trailing args are all for this target" patterns.
    ///
    /// Mutually exclusive with `consumes` and `consumes_until`; only
    /// valid when `passthrough = true`.
    #[serde(default)]
    pub consumes_rest: bool,
}

/// One or many [`InputKind`]s — mirrors [`StringOrVec`] so TOML users can
/// write `input_type = "raw"` or `input_type = ["file", "raw"]`.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum InputTypes {
    One(InputKind),
    Many(Vec<InputKind>),
}

impl InputTypes {
    pub fn contains(&self, kind: InputKind) -> bool {
        match self {
            InputTypes::One(k) => *k == kind,
            InputTypes::Many(v) => v.contains(&kind),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum StringOrVec {
    One(String),
    Many(Vec<String>),
}

impl StringOrVec {
    pub fn as_slice(&self) -> Vec<&str> {
        match self {
            StringOrVec::One(s) => vec![s.as_str()],
            StringOrVec::Many(v) => v.iter().map(String::as_str).collect(),
        }
    }
}

pub const DEFAULT_GROUP: &str = "default";
pub const DEFAULT_MODE: &str = "remote";

fn default_mode() -> String {
    DEFAULT_MODE.to_string()
}

fn is_template(s: &str) -> bool {
    s.contains("{{") || s.contains("{%")
}

/// Config + ahead-of-time regex compilation + cross-reference validation.
#[derive(Debug)]
pub struct ResolvedConfig {
    pub raw: Config,
    pub rule_regexes: Vec<Vec<Regex>>,
    /// Parallel to [`Self::rule_regexes`]. Empty Vec for rules without an
    /// `exclude` clause.
    pub rule_excludes: Vec<Vec<Regex>>,
    /// Parallel to [`Self::rule_regexes`]. `Some` when the rule has
    /// `consumes_until`, else `None`.
    pub rule_consumes_until: Vec<Option<Regex>>,
}

impl ResolvedConfig {
    pub fn rule(&self, idx: usize) -> &Rule {
        &self.raw.rules[idx]
    }

    pub fn target(&self, name: &str) -> Result<&Target> {
        self.raw
            .todoke
            .get(name)
            .ok_or_else(|| anyhow!("rule references unknown todoke target: {name}"))
    }

    fn compile(raw: Config) -> Result<Self> {
        // validate rule.to references; skip rules whose `to` is a Tera
        // template (e.g. `"{{ vars.gui }}"`) — those resolve at dispatch time
        // and the dispatcher surfaces a clear error if the rendered name is
        // still not a known target.
        for (i, rule) in raw.rules.iter().enumerate() {
            if rule.joined && rule.passthrough {
                return Err(anyhow!(
                    "rule[{i}] ({}) sets both joined = true and passthrough = true — these are mutually exclusive; joined already lets args templates place captures anywhere, so passthrough is redundant",
                    rule.name.as_deref().unwrap_or("<unnamed>"),
                ));
            }
            let consumes_forms = (rule.consumes > 0) as u8
                + rule.consumes_until.is_some() as u8
                + rule.consumes_rest as u8;
            if consumes_forms > 1 {
                return Err(anyhow!(
                    "rule[{i}] ({}) sets more than one of consumes / consumes_until / consumes_rest — pick exactly one",
                    rule.name.as_deref().unwrap_or("<unnamed>"),
                ));
            }
            if consumes_forms > 0 && !rule.passthrough {
                return Err(anyhow!(
                    "rule[{i}] ({}) has consumes* set but passthrough = false — consume options only apply to passthrough rules",
                    rule.name.as_deref().unwrap_or("<unnamed>"),
                ));
            }
            match &rule.to {
                None => {
                    if !rule.passthrough {
                        return Err(anyhow!(
                            "rule[{i}] ({}) has no `to` — only `passthrough = true` rules may omit it (they merge into another rule's batch)",
                            rule.name.as_deref().unwrap_or("<unnamed>"),
                        ));
                    }
                }
                Some(to) => {
                    if is_template(to) {
                        continue;
                    }
                    if !raw.todoke.contains_key(to) {
                        return Err(anyhow!(
                            "rule[{i}] ({}) references unknown todoke target '{}'. Known targets: {}",
                            rule.name.as_deref().unwrap_or("<unnamed>"),
                            to,
                            raw.todoke.keys().cloned().collect::<Vec<_>>().join(", ")
                        ));
                    }
                }
            }
        }

        // compile all match regexes
        let rule_regexes = raw
            .rules
            .iter()
            .enumerate()
            .map(|(i, rule)| {
                rule.match_
                    .as_slice()
                    .iter()
                    .map(|p| {
                        Regex::new(p).with_context(|| {
                            format!("rule[{i}]: failed to compile match pattern '{p}'")
                        })
                    })
                    .collect::<Result<Vec<_>>>()
            })
            .collect::<Result<Vec<_>>>()?;

        // compile all exclude regexes (empty Vec when the rule has no exclude)
        let rule_excludes = raw
            .rules
            .iter()
            .enumerate()
            .map(|(i, rule)| match &rule.exclude {
                None => Ok(Vec::new()),
                Some(patterns) => patterns
                    .as_slice()
                    .iter()
                    .map(|p| {
                        Regex::new(p).with_context(|| {
                            format!("rule[{i}]: failed to compile exclude pattern '{p}'")
                        })
                    })
                    .collect::<Result<Vec<_>>>(),
            })
            .collect::<Result<Vec<_>>>()?;

        let rule_consumes_until = raw
            .rules
            .iter()
            .enumerate()
            .map(|(i, rule)| match &rule.consumes_until {
                None => Ok(None),
                Some(p) => Regex::new(p)
                    .map(Some)
                    .with_context(|| format!("rule[{i}]: failed to compile consumes_until '{p}'")),
            })
            .collect::<Result<Vec<_>>>()?;

        Ok(Self {
            raw,
            rule_regexes,
            rule_excludes,
            rule_consumes_until,
        })
    }
}

/// Resolve which config file todoke should load.
///
/// Priority:
/// 1. Explicit `--config <path>` argument.
/// 2. `$TODOKE_CONFIG` env var.
/// 3. `~/.config/todoke/todoke.toml` on every platform. We deliberately pick
///    the XDG-style layout on Windows too (instead of `%APPDATA%\todoke\`) so
///    the same dotfiles repo works everywhere — the common setup for users
///    of chezmoi / stow / yadm, who put configs under `.config/` on all OSes.
pub fn resolve_path(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        return Ok(p.to_path_buf());
    }
    if let Ok(env_path) = std::env::var("TODOKE_CONFIG") {
        return Ok(PathBuf::from(env_path));
    }
    let home = BaseDirs::new()
        .map(|d| d.home_dir().to_path_buf())
        .ok_or_else(|| anyhow!("could not determine home directory"))?;
    Ok(home.join(".config").join("todoke").join("todoke.toml"))
}

/// Load + parse config. Falls back to the embedded default when the file does
/// not exist (but NOT when it exists and is broken — that should always error).
pub fn load(explicit: Option<&Path>) -> Result<ResolvedConfig> {
    let path = resolve_path(explicit)?;
    let (text, source) = if path.exists() {
        let t = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read config file: {}", path.display()))?;
        (t, Some(path))
    } else {
        (DEFAULT_CONFIG_TOML.to_string(), None)
    };

    let rendered = prerender(&text).with_context(|| {
        source
            .as_ref()
            .map(|p| format!("Tera pre-render failed for {}", p.display()))
            .unwrap_or_else(|| "Tera pre-render failed for embedded default TOML".into())
    })?;

    let raw: Config = toml::from_str(&rendered).with_context(|| {
        source
            .as_ref()
            .map(|p| format!("failed to parse TOML at {}", p.display()))
            .unwrap_or_else(|| "failed to parse embedded default TOML".into())
    })?;

    ResolvedConfig::compile(raw)
}

/// Alternative loader that parses from an explicit string (useful for tests).
#[allow(dead_code)]
pub fn load_from_str(text: &str) -> Result<ResolvedConfig> {
    let rendered = prerender(text).context("Tera pre-render failed")?;
    let raw: Config = toml::from_str(&rendered).context("failed to parse TOML")?;
    ResolvedConfig::compile(raw)
}

/// Pre-render the TOML text through Tera so users can use structural
/// conditionals like `{% if vars.use_neovide %}[editors.X]...{% endif %}` or
/// value-level expressions like `command = "{{ vars.gui }}"`.
///
/// The context exposes:
/// - `vars.*` — extracted from the raw text's `[vars]` / `[vars.*]` sections
///   via a lightweight line scan (so we can populate vars without having to
///   parse the whole — still-templated — file as valid TOML yet).
/// - `env.*` — process env vars.
/// - `is_windows()` / `is_linux()` / `is_mac()` — todoke-provided.
/// - Dispatch-time placeholders (`file_path`, `group`, `rule`, …) are inserted
///   as self-referential strings (`"{{ group }}"`) so those tokens pass
///   through pre-render unchanged and get rendered later with real values in
///   [`crate::dispatcher`].
pub fn prerender(text: &str) -> Result<String> {
    let vars = extract_vars(text);

    let mut tera = crate::template::new_engine();
    let mut ctx = tera::Context::new();

    let vars_map: HashMap<String, toml::Value> = vars.into_iter().collect();
    ctx.insert("vars", &vars_map);

    let env_map: HashMap<String, String> = std::env::vars().collect();
    ctx.insert("env", &env_map);

    // Self-referential placeholders keep dispatch-time tokens intact.
    for name in [
        "input",
        "input_type",
        "file_path",
        "file_dir",
        "file_name",
        "file_stem",
        "file_ext",
        "url_scheme",
        "url_host",
        "url_port",
        "url_path",
        "url_query",
        "url_fragment",
        "command_path",
        "command_dir",
        "command_name",
        "command_stem",
        "command_ext",
        "cwd",
        "group",
        "rule",
    ] {
        ctx.insert(name, &format!("{{{{ {name} }}}}"));
    }

    tera.render_str(text, &ctx).map_err(|e| {
        // Tera nests the real problem under Error::source — walk the chain so
        // the user sees the line/column, not just "Failed to parse".
        let mut msg = e.to_string();
        let mut src: Option<&(dyn std::error::Error + 'static)> = std::error::Error::source(&e);
        while let Some(s) = src {
            msg.push_str(&format!("\n  caused by: {s}"));
            src = s.source();
        }
        anyhow!(msg)
    })
}

/// Scan raw text for `[vars]` / `[vars.*]` sections and parse them as TOML.
/// Tera block lines (`{% … %}`) that may live in between sections are
/// stripped before parsing. Any parse failure yields an empty map so the
/// later pre-render pass can surface a clearer error.
fn extract_vars(text: &str) -> BTreeMap<String, toml::Value> {
    let mut buf = String::new();
    let mut in_vars = false;
    for line in text.lines() {
        let tr = line.trim_start();
        if let Some(rest) = tr.strip_prefix('[') {
            // Parse out the section name up to the closing ']'. Handles both
            // `[vars]` and `[vars.sub]`; ignores `[[array_of_tables]]`.
            let is_aot = rest.starts_with('[');
            let inner = rest
                .trim_start_matches('[')
                .split(']')
                .next()
                .unwrap_or("")
                .trim();
            in_vars = !is_aot && (inner == "vars" || inner.starts_with("vars."));
        }
        if in_vars {
            buf.push_str(line);
            buf.push('\n');
        }
    }
    if buf.is_empty() {
        return BTreeMap::new();
    }
    // Drop any Tera control blocks that slipped into buf between a [vars*]
    // section and the next section header; they are not valid TOML.
    let tera_block = Regex::new(r"(?s)\{%.*?%\}").expect("static regex");
    let cleaned = tera_block.replace_all(&buf, "");
    #[derive(Deserialize, Default)]
    struct VarsOnly {
        #[serde(default)]
        vars: BTreeMap<String, toml::Value>,
    }
    toml::from_str::<VarsOnly>(&cleaned)
        .map(|w| w.vars)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_default_config() {
        let cfg = load_from_str(DEFAULT_CONFIG_TOML).expect("default config must parse");
        assert!(cfg.raw.todoke.contains_key("nvim"));
        assert_eq!(cfg.raw.rules.len(), 2);
        assert_eq!(cfg.raw.rules[0].name.as_deref(), Some("editor-callback"));
        assert_eq!(cfg.raw.rules[1].name.as_deref(), Some("default"));
        assert!(cfg.raw.rules[0].sync);
        assert!(!cfg.raw.rules[1].sync);
    }

    #[test]
    fn rejects_unknown_to_reference() {
        let text = r#"
            [todoke.a]
            command = "echo"

            [[rules]]
            match = ".*"
            to = "does-not-exist"
        "#;
        let err = load_from_str(text).unwrap_err();
        assert!(
            err.to_string().contains("unknown todoke target"),
            "got: {err}"
        );
    }

    #[test]
    fn rejects_invalid_regex() {
        let text = r#"
            [todoke.a]
            command = "echo"

            [[rules]]
            match = "[unterminated"
            to = "a"
        "#;
        let err = load_from_str(text).unwrap_err();
        assert!(
            err.to_string().contains("failed to compile match pattern"),
            "got: {err}"
        );
    }

    #[test]
    fn rejects_multiple_consume_forms() {
        let text = r#"
            [todoke.a]
            command = "echo"

            [[rules]]
            match = '.*'
            to = "a"
            passthrough = true
            consumes = 1
            consumes_rest = true
        "#;
        let err = load_from_str(text).unwrap_err();
        assert!(err.to_string().contains("pick exactly one"), "got: {err}");
    }

    #[test]
    fn rejects_consumes_until_without_passthrough() {
        let text = r#"
            [todoke.a]
            command = "echo"

            [[rules]]
            match = '.*'
            to = "a"
            consumes_until = '^[-+]'
        "#;
        let err = load_from_str(text).unwrap_err();
        assert!(
            err.to_string().contains("consume options only apply"),
            "got: {err}"
        );
    }

    #[test]
    fn rejects_invalid_consumes_until_regex() {
        let text = r#"
            [todoke.a]
            command = "echo"

            [[rules]]
            match = '.*'
            to = "a"
            passthrough = true
            consumes_until = '[unterminated'
        "#;
        let err = load_from_str(text).unwrap_err();
        assert!(err.to_string().contains("consumes_until"), "got: {err}");
    }

    #[test]
    fn passthrough_rule_can_omit_to() {
        let text = r#"
            [todoke.a]
            command = "echo"

            [[rules]]
            name = "any-flag"
            match = '^-'
            passthrough = true
        "#;
        let cfg = load_from_str(text).expect("passthrough rule should allow omitted `to`");
        assert!(cfg.raw.rules[0].to.is_none());
    }

    #[test]
    fn non_passthrough_rule_requires_to() {
        let text = r#"
            [todoke.a]
            command = "echo"

            [[rules]]
            name = "orphan"
            match = '.*'
        "#;
        let err = load_from_str(text).unwrap_err();
        assert!(err.to_string().contains("has no `to`"), "got: {err}");
    }

    #[test]
    fn rejects_consumes_without_passthrough() {
        let text = r#"
            [todoke.a]
            command = "echo"

            [[rules]]
            match = '.*'
            to = "a"
            consumes = 1
        "#;
        let err = load_from_str(text).unwrap_err();
        assert!(
            err.to_string().contains("consume options only apply"),
            "got: {err}"
        );
    }

    #[test]
    fn rejects_joined_and_passthrough_both_true() {
        let text = r#"
            [todoke.a]
            command = "echo"

            [[rules]]
            match = '.*'
            to = "a"
            joined = true
            passthrough = true
        "#;
        let err = load_from_str(text).unwrap_err();
        assert!(err.to_string().contains("mutually exclusive"), "got: {err}");
    }

    #[test]
    fn mode_defaults_to_remote_kind_defaults_to_exec() {
        let text = r#"
            [todoke.a]
            command = "echo"

            [[rules]]
            match = ".*"
            to = "a"
        "#;
        let cfg = load_from_str(text).unwrap();
        assert_eq!(cfg.raw.rules[0].mode, "remote");
        assert!(!cfg.raw.rules[0].sync);
        assert!(cfg.raw.rules[0].group.is_none());
        assert_eq!(cfg.raw.todoke["a"].kind, TargetKind::Exec);
        // gui is a new public field; lock in the backward-compatible default.
        assert!(!cfg.raw.todoke["a"].gui);
    }

    #[test]
    fn target_gui_parses_true() {
        let text = r#"
            [todoke.a]
            command = "neovide"
            gui = true

            [[rules]]
            match = ".*"
            to = "a"
        "#;
        let cfg = load_from_str(text).unwrap();
        assert!(cfg.raw.todoke["a"].gui);
    }

    #[test]
    fn args_per_mode_with_default_fallback() {
        let text = r#"
            [todoke.a]
            command = "echo"
            [todoke.a.args]
            remote = ["--reuse"]
            default = ["--fallback"]

            [[rules]]
            match = ".*"
            to = "a"
        "#;
        let cfg = load_from_str(text).unwrap();
        let t = &cfg.raw.todoke["a"];
        assert_eq!(t.args_for("remote"), &["--reuse".to_string()]);
        assert_eq!(t.args_for("new"), &["--fallback".to_string()]);
        assert_eq!(t.args_for("anything-else"), &["--fallback".to_string()]);
    }

    #[test]
    fn tera_conditional_blocks_are_applied_at_load_time() {
        let src = r#"
            [vars]
            use_neovide = true

            [todoke.nvim]
            kind = "neovim"
            command = "nvim"
            listen = "/tmp/sock"

            {% if vars.use_neovide %}
            [todoke.nvim-gui]
            kind = "neovim"
            command = "neovide"
            listen = "/tmp/sock-gui"
            [todoke.nvim-gui.args]
            remote = ["--"]
            {% endif %}

            [[rules]]
            match = ".*"
            to = "nvim"
        "#;
        let cfg = load_from_str(src).unwrap();
        assert!(cfg.raw.todoke.contains_key("nvim-gui"));

        let src_off = src.replace("use_neovide = true", "use_neovide = false");
        let cfg2 = load_from_str(&src_off).unwrap();
        assert!(!cfg2.raw.todoke.contains_key("nvim-gui"));
        assert!(cfg2.raw.todoke.contains_key("nvim"));
    }

    #[test]
    fn dispatch_time_placeholders_survive_prerender() {
        let src = r#"
            [todoke.nvim]
            kind = "neovim"
            command = "nvim"
            listen = '/tmp/nvim-todoke-{{ group }}.sock'

            [[rules]]
            match = ".*"
            to = "nvim"
            group = "{{ file_stem }}"
        "#;
        let cfg = load_from_str(src).unwrap();
        assert_eq!(
            cfg.raw.todoke["nvim"].listen.as_deref(),
            Some("/tmp/nvim-todoke-{{ group }}.sock"),
        );
        assert_eq!(cfg.raw.rules[0].group.as_deref(), Some("{{ file_stem }}"));
    }

    #[test]
    fn vars_value_substitutes_top_level() {
        let src = r#"
            [vars]
            gui = "neovide"

            [todoke.nvim]
            kind = "neovim"
            command = "{{ vars.gui }}"
            listen = "/tmp/sock"

            [[rules]]
            match = ".*"
            to = "nvim"
        "#;
        let cfg = load_from_str(src).unwrap();
        assert_eq!(cfg.raw.todoke["nvim"].command, "neovide");
    }

    #[test]
    fn vars_subtables_are_picked_up() {
        let src = r#"
            [vars]
            gui = "neovide"

            [vars.colors]
            primary = "blue"

            [todoke.nvim]
            kind = "neovim"
            command = "{{ vars.gui }}"
            listen = "/tmp/{{ vars.colors.primary }}"

            [[rules]]
            match = ".*"
            to = "nvim"
        "#;
        let cfg = load_from_str(src).unwrap();
        assert_eq!(cfg.raw.todoke["nvim"].command, "neovide");
        assert_eq!(cfg.raw.todoke["nvim"].listen.as_deref(), Some("/tmp/blue"));
    }
}
