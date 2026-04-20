//! TOML + Tera config schema.
//!
//! Two layers:
//! - [`Config`]: the raw TOML deserialization target.
//! - [`ResolvedConfig`]: [`Config`] + pre-compiled regex patterns + validated
//!   cross-references. Everything you actually want to use at dispatch time.
//!
//! Tera expansion happens at dispatch time (not load time) because rule.group
//! and editor.* templates can reference per-file context.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, anyhow};
use directories::BaseDirs;
use regex::Regex;
use serde::Deserialize;

pub const DEFAULT_CONFIG_TOML: &str = include_str!("../assets/default.toml");

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub vars: BTreeMap<String, toml::Value>,
    #[serde(default)]
    pub editors: BTreeMap<String, EditorDef>,
    #[serde(default)]
    pub rules: Vec<Rule>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EditorDef {
    pub kind: EditorKind,
    pub command: String,
    #[serde(default)]
    pub listen: Option<String>,
    #[serde(default)]
    pub args_new: Vec<String>,
    #[serde(default)]
    pub args_remote: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EditorKind {
    Neovim,
    Generic,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Rule {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(rename = "match")]
    pub match_: StringOrVec,
    pub editor: String,
    #[serde(default)]
    pub group: Option<String>,
    #[serde(default)]
    pub mode: Mode,
    #[serde(default)]
    pub sync: bool,
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

#[derive(Debug, Clone, Copy, Deserialize, Default, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    #[default]
    Remote,
    New,
}

pub const DEFAULT_GROUP: &str = "default";

fn is_template(s: &str) -> bool {
    s.contains("{{") || s.contains("{%")
}

/// Config + ahead-of-time regex compilation + cross-reference validation.
#[derive(Debug)]
pub struct ResolvedConfig {
    pub raw: Config,
    pub rule_regexes: Vec<Vec<Regex>>,
}

impl ResolvedConfig {
    pub fn rule(&self, idx: usize) -> &Rule {
        &self.raw.rules[idx]
    }

    pub fn editor(&self, name: &str) -> Result<&EditorDef> {
        self.raw
            .editors
            .get(name)
            .ok_or_else(|| anyhow!("rule references unknown editor: {name}"))
    }

    fn compile(raw: Config) -> Result<Self> {
        // validate editor references; skip rules whose editor field is a Tera
        // template (e.g. `"{{ vars.gui }}"`) — those resolve at dispatch time
        // and the dispatcher surfaces a clear error if the rendered name is
        // still not a known editor.
        for (i, rule) in raw.rules.iter().enumerate() {
            if is_template(&rule.editor) {
                continue;
            }
            if !raw.editors.contains_key(&rule.editor) {
                return Err(anyhow!(
                    "rule[{i}] ({}) references unknown editor '{}'. Known editors: {}",
                    rule.name.as_deref().unwrap_or("<unnamed>"),
                    rule.editor,
                    raw.editors.keys().cloned().collect::<Vec<_>>().join(", ")
                ));
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

        Ok(Self { raw, rule_regexes })
    }
}

/// Resolve which config file edtr should load.
///
/// Priority:
/// 1. Explicit `--config <path>` argument.
/// 2. `$EDTR_CONFIG` env var.
/// 3. `~/.config/edtr/edtr.toml` on every platform. We deliberately pick the
///    XDG-style layout on Windows too (instead of `%APPDATA%\edtr\`) so the
///    same dotfiles repo works everywhere — the common setup for users of
///    chezmoi / stow / yadm, who put configs under `.config/` on all OSes.
pub fn resolve_path(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        return Ok(p.to_path_buf());
    }
    if let Ok(env_path) = std::env::var("EDTR_CONFIG") {
        return Ok(PathBuf::from(env_path));
    }
    let home = BaseDirs::new()
        .map(|d| d.home_dir().to_path_buf())
        .ok_or_else(|| anyhow!("could not determine home directory"))?;
    Ok(home.join(".config").join("edtr").join("edtr.toml"))
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
/// - `is_windows()` / `is_linux()` / `is_mac()` — edtr-provided.
/// - Dispatch-time placeholders (`file_path`, `group`, `rule`, …) are inserted
///   as self-referential strings (`"{{ group }}"`) so those tokens pass
///   through pre-render unchanged and get rendered later with real values in
///   [`crate::dispatcher`].
fn prerender(text: &str) -> Result<String> {
    let vars = extract_vars(text);

    let mut tera = crate::template::new_engine();
    let mut ctx = tera::Context::new();

    let vars_map: HashMap<String, toml::Value> = vars.into_iter().collect();
    ctx.insert("vars", &vars_map);

    let env_map: HashMap<String, String> = std::env::vars().collect();
    ctx.insert("env", &env_map);

    // Self-referential placeholders keep dispatch-time tokens intact.
    for name in [
        "file_path",
        "file_dir",
        "file_name",
        "file_stem",
        "file_ext",
        "editor_path",
        "editor_dir",
        "editor_name",
        "editor_stem",
        "editor_ext",
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
        assert!(cfg.raw.editors.contains_key("nvim"));
        assert_eq!(cfg.raw.rules.len(), 2);
        assert_eq!(cfg.raw.rules[0].name.as_deref(), Some("editor-callback"));
        assert_eq!(cfg.raw.rules[1].name.as_deref(), Some("default"));
        assert!(cfg.raw.rules[0].sync);
        assert!(!cfg.raw.rules[1].sync);
    }

    #[test]
    fn rejects_unknown_editor_reference() {
        let text = r#"
            [editors.a]
            kind = "generic"
            command = "echo"

            [[rules]]
            match = ".*"
            editor = "does-not-exist"
        "#;
        let err = load_from_str(text).unwrap_err();
        assert!(err.to_string().contains("unknown editor"), "got: {err}");
    }

    #[test]
    fn rejects_invalid_regex() {
        let text = r#"
            [editors.a]
            kind = "generic"
            command = "echo"

            [[rules]]
            match = "[unterminated"
            editor = "a"
        "#;
        let err = load_from_str(text).unwrap_err();
        assert!(
            err.to_string().contains("failed to compile match pattern"),
            "got: {err}"
        );
    }

    #[test]
    fn mode_defaults_to_remote() {
        let text = r#"
            [editors.a]
            kind = "generic"
            command = "echo"

            [[rules]]
            match = ".*"
            editor = "a"
        "#;
        let cfg = load_from_str(text).unwrap();
        assert_eq!(cfg.raw.rules[0].mode, Mode::Remote);
        assert!(!cfg.raw.rules[0].sync);
        assert!(cfg.raw.rules[0].group.is_none());
    }

    #[test]
    fn tera_conditional_blocks_are_applied_at_load_time() {
        // Same source, different vars → different rule set.
        let src = r#"
            [vars]
            use_neovide = true

            [editors.nvim]
            kind = "neovim"
            command = "nvim"
            listen = "/tmp/sock"

            {% if vars.use_neovide %}
            [editors.nvim-gui]
            kind = "neovim"
            command = "neovide"
            listen = "/tmp/sock-gui"
            args_remote = ["--"]
            {% endif %}

            [[rules]]
            match = ".*"
            editor = "nvim"
        "#;
        let cfg = load_from_str(src).unwrap();
        assert!(cfg.raw.editors.contains_key("nvim-gui"));

        let src_off = src.replace("use_neovide = true", "use_neovide = false");
        let cfg2 = load_from_str(&src_off).unwrap();
        assert!(!cfg2.raw.editors.contains_key("nvim-gui"));
        assert!(cfg2.raw.editors.contains_key("nvim"));
    }

    #[test]
    fn dispatch_time_placeholders_survive_prerender() {
        // `{{ group }}` and `{{ file_path }}` must pass through pre-render
        // intact so the dispatcher can fill them per file later.
        let src = r#"
            [editors.nvim]
            kind = "neovim"
            command = "nvim"
            listen = '/tmp/nvim-edtr-{{ group }}.sock'

            [[rules]]
            match = ".*"
            editor = "nvim"
            group = "{{ file_stem }}"
        "#;
        let cfg = load_from_str(src).unwrap();
        assert_eq!(
            cfg.raw.editors["nvim"].listen.as_deref(),
            Some("/tmp/nvim-edtr-{{ group }}.sock"),
        );
        assert_eq!(cfg.raw.rules[0].group.as_deref(), Some("{{ file_stem }}"));
    }

    #[test]
    fn vars_value_substitutes_top_level() {
        let src = r#"
            [vars]
            gui = "neovide"

            [editors.nvim]
            kind = "neovim"
            command = "{{ vars.gui }}"
            listen = "/tmp/sock"

            [[rules]]
            match = ".*"
            editor = "nvim"
        "#;
        let cfg = load_from_str(src).unwrap();
        assert_eq!(cfg.raw.editors["nvim"].command, "neovide");
    }

    #[test]
    fn vars_subtables_are_picked_up() {
        let src = r#"
            [vars]
            gui = "neovide"

            [vars.colors]
            primary = "blue"

            [editors.nvim]
            kind = "neovim"
            command = "{{ vars.gui }}"
            listen = "/tmp/{{ vars.colors.primary }}"

            [[rules]]
            match = ".*"
            editor = "nvim"
        "#;
        let cfg = load_from_str(src).unwrap();
        assert_eq!(cfg.raw.editors["nvim"].command, "neovide");
        assert_eq!(cfg.raw.editors["nvim"].listen.as_deref(), Some("/tmp/blue"));
    }
}
