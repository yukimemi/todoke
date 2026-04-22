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

/// Custom-scheme identifier like `issue:42`, `gh:owner/repo`, `jira:ABC-1`.
/// Used by the auto-detector to route these to [`Input::Raw`] so rules can
/// match their scheme prefix.
///
/// Requires a 2+ char "scheme" so Windows drive letters (`C:\foo`, `D:bar`)
/// don't trip — those fall through to [`Input::File`].
fn custom_scheme_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[A-Za-z][A-Za-z0-9+\-.]+:[^\\/]").expect("static regex"))
}

pub fn looks_like_custom_scheme(s: &str) -> bool {
    custom_scheme_re().is_match(s)
}

/// Chars that cannot legally appear in a Windows filename (and are
/// unusual enough on POSIX that treating them as a signal is safe).
///
/// `:` is intentionally excluded — drive letters use it, and
/// [`looks_like_custom_scheme`] already routed `<scheme>:<body>` to Raw.
/// Path separators (`/`, `\`) are also excluded since they're how
/// directory components are written.
pub fn has_invalid_path_chars(s: &str) -> bool {
    s.chars()
        .any(|c| matches!(c, '<' | '>' | '"' | '|' | '?' | '*') || (c as u32) < 0x20)
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

/// Explicit override for how to classify an input — used by `--as` and by
/// `rule.input_type` to filter which kinds a rule applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum, serde::Deserialize)]
#[clap(rename_all = "lower")]
#[serde(rename_all = "lowercase")]
pub enum InputKind {
    File,
    Url,
    Raw,
}

impl Input {
    /// Classify `raw` and do the minimum parsing / canonicalization.
    ///
    /// Auto-detection order:
    /// 1. URL scheme (`foo://…`)
    /// 2. Existing filesystem path (canonicalized)
    /// 3. Custom-scheme bare identifier (`issue:42`, `gh:owner/repo`) → Raw
    /// 4. Contains chars no filename can hold (`<>"|?*`, controls) or is
    ///    empty → Raw
    /// 5. Everything else (bare words, extension-less names, not-yet-existing
    ///    paths) → File, absolutized relative to the cwd
    ///
    /// Tier 5 is "when in doubt, it's a file": `Makefile`, `Dockerfile`,
    /// `newfile.txt`, `./foo` all classify as File so editors can open or
    /// create them without the user having to pass `--as file`. Use
    /// `--as raw` or `rule.input_type` to route specific bare words to
    /// a Raw-consuming rule instead.
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
                // `--as file` must not fail on nonexistent paths — absolutize
                // without canonicalize so the "create this file" flow works.
                let p = PathBuf::from(raw)
                    .canonicalize()
                    .or_else(|_| std::path::absolute(raw))
                    .unwrap_or_else(|_| PathBuf::from(raw));
                Ok(Input::File(p))
            }
            Some(InputKind::Raw) => Ok(Input::Raw(raw.to_string())),
            None => {
                if looks_like_url(raw) {
                    let u = url::Url::parse(raw).with_context(|| format!("invalid URL: {raw}"))?;
                    return Ok(Input::Url(u));
                }
                if let Ok(p) = PathBuf::from(raw).canonicalize() {
                    return Ok(Input::File(p));
                }
                if looks_like_custom_scheme(raw) {
                    return Ok(Input::Raw(raw.to_string()));
                }
                // Contains chars that can't appear in a filename (`<>"|?*`,
                // controls) or is empty — not a plausible path, route to Raw.
                if raw.is_empty() || has_invalid_path_chars(raw) {
                    return Ok(Input::Raw(raw.to_string()));
                }
                let abs = std::path::absolute(raw).unwrap_or_else(|_| PathBuf::from(raw));
                Ok(Input::File(abs))
            }
        }
    }

    pub fn kind(&self) -> InputKind {
        match self {
            Input::File(_) => InputKind::File,
            Input::Url(_) => InputKind::Url,
            Input::Raw(_) => InputKind::Raw,
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
    fn custom_scheme_args_classify_as_raw() {
        // `<scheme>:<body>` with a 2+ char scheme is treated as a custom
        // routing identifier, not a file.
        let i = Input::from_arg("issue:1234").unwrap();
        assert!(matches!(i, Input::Raw(_)));
        assert_eq!(i.kind_label(), "raw");
        assert_eq!(i.match_string(), "issue:1234");

        let i = Input::from_arg("gh:owner/repo").unwrap();
        assert!(matches!(i, Input::Raw(_)));
    }

    #[test]
    fn nonexistent_path_like_args_classify_as_file() {
        // $EDITOR new-file use case: the file doesn't exist yet but we
        // still treat it as a file so the editor can create it.
        for s in [
            "/tmp/does-not-exist-todoke-test.md",
            "newfile.txt",
            "./relative-new.log",
        ] {
            let i = Input::from_arg(s).unwrap();
            assert!(matches!(i, Input::File(_)), "{s} should be File");
        }
    }

    #[test]
    fn extensionless_bare_words_classify_as_file() {
        // Makefile / Dockerfile / Rakefile / HEAD / plain words all default
        // to File now — so `$EDITOR=todoke Makefile` Just Works and rules
        // that want to treat bare words as Raw must opt in via
        // `input_type = "raw"` (or the caller uses `--as raw`).
        for s in ["Makefile", "Dockerfile", "HEAD", "main", "some-bare-word"] {
            let i = Input::from_arg(s).unwrap();
            assert!(matches!(i, Input::File(_)), "{s} should be File");
        }
    }

    #[test]
    fn invalid_path_chars_route_to_raw() {
        // Chars that can't appear in a Windows filename mean the arg
        // isn't a plausible path — classify as Raw so rules can match
        // things like shell-ish free text or wildcards.
        for s in ["foo|bar", "a?b", "star*arg", "quote\"it", "<tag>"] {
            let i = Input::from_arg(s).unwrap();
            assert!(matches!(i, Input::Raw(_)), "{s} should be Raw");
        }
    }

    #[test]
    fn has_invalid_path_chars_detects_reserved() {
        assert!(has_invalid_path_chars("foo|bar"));
        assert!(has_invalid_path_chars("a?b"));
        assert!(has_invalid_path_chars("a\x01b"));
        // Path separators and drive-letter colons are fine.
        assert!(!has_invalid_path_chars("C:\\Users\\x"));
        assert!(!has_invalid_path_chars("/abs/path"));
        assert!(!has_invalid_path_chars("Makefile"));
    }

    #[test]
    fn looks_like_custom_scheme_cases() {
        assert!(looks_like_custom_scheme("issue:42"));
        assert!(looks_like_custom_scheme("gh:owner/repo"));
        assert!(looks_like_custom_scheme("jira:ABC-1"));
        // 2+ char scheme requirement excludes Windows drive letters.
        assert!(!looks_like_custom_scheme("C:\\Users\\x"));
        assert!(!looks_like_custom_scheme("D:foo"));
        // No colon at all.
        assert!(!looks_like_custom_scheme("Makefile"));
        assert!(!looks_like_custom_scheme("HEAD"));
        // Colon followed by a path separator is still drive-letter-ish.
        assert!(!looks_like_custom_scheme("scheme:/slash"));
    }

    #[test]
    fn force_as_file_absolutizes_nonexistent() {
        // `--as file NONEXISTENT` must not error; it should absolutize the
        // path so downstream editors can create the file.
        let i = Input::from_arg_as("nonexistent-todoke-test.md", Some(InputKind::File)).unwrap();
        assert!(matches!(i, Input::File(_)));
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
