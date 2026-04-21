//! Input abstraction: every argument todoke receives is either a file path
//! or a URL. They're matched against rules with the same regex engine but
//! have different template contexts and different backend compatibility
//! (the neovim backend can only open files).

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Context as _, Result};
use regex::Regex;

/// RFC 3986 scheme: ALPHA *( ALPHA / DIGIT / "+" / "-" / "." ) followed by `://`.
/// We use `://` rather than just `:` so Windows drive letters (`C:\foo`)
/// aren't misclassified as URLs.
fn url_scheme_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[A-Za-z][A-Za-z0-9+\-.]*://").expect("static regex"))
}

pub fn looks_like_url(s: &str) -> bool {
    url_scheme_re().is_match(s)
}

#[derive(Debug, Clone)]
pub enum Input {
    File(PathBuf),
    Url(url::Url),
    /// Arbitrary string — anything that's not a URL and doesn't resolve to
    /// an existing file on disk. Opens the door to routing non-path things
    /// like `issue:123`, `gh:owner/repo`, `HEAD`, free text, etc.
    Raw(String),
}

/// Explicit override for how to classify an input — used by `--as`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
#[clap(rename_all = "lower")]
pub enum InputKind {
    File,
    Url,
    Raw,
}

impl Input {
    /// Classify `raw` and do the minimum parsing / canonicalization.
    /// Auto-detection order: URL scheme → existing file → raw.
    #[allow(dead_code)]
    pub fn from_arg(raw: &str) -> Result<Self> {
        Self::from_arg_as(raw, None)
    }

    /// Same as [`Self::from_arg`] but accepts an explicit override from
    /// `--as <kind>`.
    pub fn from_arg_as(raw: &str, force: Option<InputKind>) -> Result<Self> {
        match force {
            Some(InputKind::Url) => {
                let u = url::Url::parse(raw).with_context(|| format!("invalid URL: {raw}"))?;
                Ok(Input::Url(u))
            }
            Some(InputKind::File) => {
                let p = PathBuf::from(raw)
                    .canonicalize()
                    .with_context(|| format!("cannot resolve path: {raw}"))?;
                Ok(Input::File(p))
            }
            Some(InputKind::Raw) => Ok(Input::Raw(raw.to_string())),
            None => {
                if looks_like_url(raw) {
                    let u = url::Url::parse(raw).with_context(|| format!("invalid URL: {raw}"))?;
                    return Ok(Input::Url(u));
                }
                match PathBuf::from(raw).canonicalize() {
                    Ok(p) => Ok(Input::File(p)),
                    Err(_) => Ok(Input::Raw(raw.to_string())),
                }
            }
        }
    }

    /// String used for regex matching. Files are normalized (see
    /// [`crate::matcher::normalize_path`]); URLs and raw strings are matched
    /// as-is.
    pub fn match_string(&self) -> String {
        match self {
            Input::File(p) => crate::matcher::normalize_path(p),
            Input::Url(u) => u.as_str().to_string(),
            Input::Raw(s) => s.clone(),
        }
    }

    /// The raw, user-facing string — what users see when todoke logs about
    /// this input and what gets substituted into `{{ input }}`.
    pub fn display_string(&self) -> String {
        match self {
            Input::File(p) => crate::matcher::strip_verbatim(&p.to_string_lossy()),
            Input::Url(u) => u.as_str().to_string(),
            Input::Raw(s) => s.clone(),
        }
    }

    pub fn as_file(&self) -> Option<&Path> {
        match self {
            Input::File(p) => Some(p),
            _ => None,
        }
    }

    #[allow(dead_code)]
    pub fn as_url(&self) -> Option<&url::Url> {
        match self {
            Input::Url(u) => Some(u),
            _ => None,
        }
    }

    #[allow(dead_code)]
    pub fn as_raw(&self) -> Option<&str> {
        match self {
            Input::Raw(s) => Some(s.as_str()),
            _ => None,
        }
    }

    pub fn kind_label(&self) -> &'static str {
        match self {
            Input::File(_) => "file",
            Input::Url(_) => "url",
            Input::Raw(_) => "raw",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_detection() {
        assert!(looks_like_url("http://example.com"));
        assert!(looks_like_url("https://example.com/path"));
        assert!(looks_like_url("ftp://host"));
        assert!(looks_like_url("file:///etc/hosts"));
        assert!(!looks_like_url("C:\\Users\\x\\file.txt"));
        assert!(!looks_like_url("/home/x/file.txt"));
        assert!(!looks_like_url("./relative"));
    }

    #[test]
    fn raw_fallback_when_not_url_or_file() {
        // A clearly non-existent path that isn't a URL falls through to Raw.
        let i = Input::from_arg("issue:1234").unwrap();
        assert!(matches!(i, Input::Raw(_)));
        assert_eq!(i.kind_label(), "raw");
        assert_eq!(i.match_string(), "issue:1234");
    }

    #[test]
    fn force_as_raw_skips_canonicalize() {
        // Force raw even for something that could resolve to a file.
        let i = Input::from_arg_as(".", Some(InputKind::Raw)).unwrap();
        assert!(matches!(i, Input::Raw(_)));
        assert_eq!(i.display_string(), ".");
    }

    #[test]
    fn url_still_detected_first() {
        let i = Input::from_arg("https://example.com/foo").unwrap();
        assert!(matches!(i, Input::Url(_)));
    }
}
