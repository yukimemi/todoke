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
}

impl Input {
    /// Classify `raw` and do the minimum parsing / canonicalization.
    pub fn from_arg(raw: &str) -> Result<Self> {
        if looks_like_url(raw) {
            let u = url::Url::parse(raw).with_context(|| format!("invalid URL: {raw}"))?;
            return Ok(Input::Url(u));
        }
        let p = PathBuf::from(raw)
            .canonicalize()
            .with_context(|| format!("cannot resolve path: {raw}"))?;
        Ok(Input::File(p))
    }

    /// String used for regex matching. Files are normalized (see
    /// [`crate::matcher::normalize_path`]); URLs are matched as-is.
    pub fn match_string(&self) -> String {
        match self {
            Input::File(p) => crate::matcher::normalize_path(p),
            Input::Url(u) => u.as_str().to_string(),
        }
    }

    /// The raw, user-facing string — what users see when todoke logs about
    /// this input and what gets substituted into `{{ input }}`.
    pub fn display_string(&self) -> String {
        match self {
            Input::File(p) => crate::matcher::strip_verbatim(&p.to_string_lossy()),
            Input::Url(u) => u.as_str().to_string(),
        }
    }

    pub fn as_file(&self) -> Option<&Path> {
        match self {
            Input::File(p) => Some(p),
            Input::Url(_) => None,
        }
    }

    #[allow(dead_code)]
    pub fn as_url(&self) -> Option<&url::Url> {
        match self {
            Input::Url(u) => Some(u),
            Input::File(_) => None,
        }
    }

    pub fn kind_label(&self) -> &'static str {
        match self {
            Input::File(_) => "file",
            Input::Url(_) => "url",
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
}
