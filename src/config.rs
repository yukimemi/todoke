//! TOML + Tera config schema.
//!
//! Two layers:
//! - [`Config`]: the raw TOML deserialization target.
//! - [`ResolvedConfig`]: [`Config`] + pre-compiled regex patterns + validated
//!   cross-references. Everything you actually want to use at dispatch time.
//!
//! Tera expansion happens at dispatch time (not load time) because rule.group
//! and editor.* templates can reference per-file context.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, anyhow};
use directories::ProjectDirs;
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
        // validate editor references
        for (i, rule) in raw.rules.iter().enumerate() {
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
/// 1. Explicit --config <path> argument
/// 2. `$EDTR_CONFIG` env var
/// 3. `~/.config/edtr/edtr.toml` on Linux/Mac or
///    `%APPDATA%\edtr\edtr.toml` on Windows (via `directories` crate)
pub fn resolve_path(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        return Ok(p.to_path_buf());
    }
    if let Ok(env_path) = std::env::var("EDTR_CONFIG") {
        return Ok(PathBuf::from(env_path));
    }
    let dirs = ProjectDirs::from("", "", "edtr")
        .ok_or_else(|| anyhow!("could not determine config directory for edtr"))?;
    Ok(dirs.config_dir().join("edtr.toml"))
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

    let raw: Config = toml::from_str(&text).with_context(|| {
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
    let raw: Config = toml::from_str(text).context("failed to parse TOML")?;
    ResolvedConfig::compile(raw)
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
}
