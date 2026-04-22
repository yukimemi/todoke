//! Regex-based rule matcher. First-match-wins across [[rules]].
//!
//! Patterns are pre-compiled in [`crate::config::ResolvedConfig::compile`] so
//! hot-path matching is cheap. A rule matches if ANY of its `match` regexes
//! hit AND NONE of its `exclude` regexes hit.
//!
//! When a rule matches, the capture groups of the specific `match` regex
//! that hit are returned so the dispatcher can expose them to templates as
//! `{{ cap.N }}` / `{{ cap.<name> }}`.

use std::collections::BTreeMap;
use std::path::Path;

use crate::config::ResolvedConfig;
use crate::input::InputKind;

/// Ordered map of capture name → captured text. Numbered groups use string
/// keys `"0"`, `"1"`, ...; named groups use their declared name. The full
/// match is always at `"0"`.
pub type CaptureMap = BTreeMap<String, String>;

/// Strip Windows verbatim prefixes, converting `\\?\UNC\server\share\...` to
/// `\\server\share\...` and `\\?\C:\...` to `C:\...`. Everything else is
/// returned unchanged.
pub fn strip_verbatim(s: &str) -> String {
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}");
    }
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        return rest.to_string();
    }
    s.to_string()
}

/// Normalize a path for regex matching.
///
/// - Strips Windows `\\?\` verbatim prefixes (including `\\?\UNC\`).
/// - Converts backslashes to forward slashes.
///
/// A UNC path `\\server\share\file.txt` becomes `//server/share/file.txt`.
/// Local paths are in the same forward-slash form, giving users a single
/// canonical form to write regexes against.
pub fn normalize_path(p: &Path) -> String {
    strip_verbatim(&p.to_string_lossy()).replace('\\', "/")
}

/// Path form safe to pass to `nvim :edit`.
///
/// For UNC paths (`\\server\share\...`) we keep backslashes because nvim on
/// Windows goes through libuv which expects the native UNC form. For normal
/// paths we convert to forward slashes (nvim handles both, and slashes avoid
/// escape issues inside `:edit <path>`).
pub fn vim_path(p: &Path) -> String {
    let stripped = strip_verbatim(&p.to_string_lossy());
    if stripped.starts_with(r"\\") {
        stripped
    } else {
        stripped.replace('\\', "/")
    }
}

/// Find the first rule whose patterns match `subject`. A rule counts as
/// matching when ANY of its `match` patterns hits AND NONE of its `exclude`
/// patterns hits. The subject is whatever string form the caller chose
/// (normalized file path, raw URL, raw string) — see [`crate::input::Input`].
///
/// When `kind` is `Some(k)`, rules with an `input_type` clause that does not
/// include `k` are skipped even if their patterns would match. `None` means
/// "don't filter by kind" (used by the no-args / empty-subject path).
///
/// Returns the matching rule's index plus the [`CaptureMap`] extracted from
/// the specific match regex that hit (empty map when nothing captured).
pub fn first_match(
    cfg: &ResolvedConfig,
    subject: &str,
    kind: Option<InputKind>,
) -> Option<(usize, CaptureMap)> {
    for (i, regexes) in cfg.rule_regexes.iter().enumerate() {
        if let Some(k) = kind {
            if let Some(allowed) = &cfg.raw.rules[i].input_type {
                if !allowed.contains(k) {
                    continue;
                }
            }
        }
        let hit = regexes
            .iter()
            .find_map(|re| re.captures(subject).map(|c| (re, c)));
        let Some((re, caps)) = hit else {
            continue;
        };
        if cfg.rule_excludes[i].iter().any(|r| r.is_match(subject)) {
            continue;
        }

        let mut map = CaptureMap::new();
        for idx in 0..caps.len() {
            if let Some(m) = caps.get(idx) {
                map.insert(idx.to_string(), m.as_str().to_string());
            }
        }
        for name in re.capture_names().flatten() {
            if let Some(m) = caps.name(name) {
                map.insert(name.to_string(), m.as_str().to_string());
            }
        }
        return Some((i, map));
    }
    None
}

/// Shorter form for callers that don't want captures.
#[allow(dead_code)]
pub fn first_match_idx(cfg: &ResolvedConfig, subject: &str) -> Option<usize> {
    first_match(cfg, subject, None).map(|(i, _)| i)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::load_from_str;
    use std::path::PathBuf;

    #[test]
    fn normalize_strips_verbatim_prefix() {
        let p = PathBuf::from(r"\\?\C:\Users\x\file.txt");
        assert_eq!(normalize_path(&p), "C:/Users/x/file.txt");
    }

    #[test]
    fn normalize_converts_backslashes() {
        let p = PathBuf::from(r"C:\Users\x\file.txt");
        assert_eq!(normalize_path(&p), "C:/Users/x/file.txt");
    }

    #[test]
    fn normalize_unix_passthrough() {
        let p = PathBuf::from("/home/x/file.txt");
        assert_eq!(normalize_path(&p), "/home/x/file.txt");
    }

    #[test]
    fn normalize_unc_verbatim() {
        // \\?\UNC\server\share\file -> //server/share/file (for regex)
        let p = PathBuf::from(r"\\?\UNC\server\share\file.txt");
        assert_eq!(normalize_path(&p), "//server/share/file.txt");
    }

    #[test]
    fn normalize_unc_plain() {
        // \\server\share\file -> //server/share/file
        let p = PathBuf::from(r"\\server\share\file.txt");
        assert_eq!(normalize_path(&p), "//server/share/file.txt");
    }

    #[test]
    fn vim_path_preserves_unc_backslashes() {
        let p = PathBuf::from(r"\\server\share\file.txt");
        assert_eq!(vim_path(&p), r"\\server\share\file.txt");
    }

    #[test]
    fn vim_path_converts_verbatim_unc() {
        let p = PathBuf::from(r"\\?\UNC\server\share\file.txt");
        assert_eq!(vim_path(&p), r"\\server\share\file.txt");
    }

    #[test]
    fn vim_path_local_uses_forward_slashes() {
        let p = PathBuf::from(r"C:\Users\x\file.txt");
        assert_eq!(vim_path(&p), "C:/Users/x/file.txt");
    }

    #[test]
    fn vim_path_strips_verbatim_local() {
        let p = PathBuf::from(r"\\?\C:\Users\x\file.txt");
        assert_eq!(vim_path(&p), "C:/Users/x/file.txt");
    }

    #[test]
    fn first_match_returns_first_hit() {
        let text = r#"
            [todoke.a]
            command = "echo"

            [[rules]]
            name = "rs"
            match = '\.rs$'
            to = "a"

            [[rules]]
            name = "any"
            match = '.*'
            to = "a"
        "#;
        let cfg = load_from_str(text).unwrap();
        assert_eq!(first_match_idx(&cfg, "/tmp/foo.rs"), Some(0));
        assert_eq!(first_match_idx(&cfg, "/tmp/README"), Some(1));
    }

    #[test]
    fn first_match_none_when_no_rule_fits() {
        let text = r#"
            [todoke.a]
            command = "echo"

            [[rules]]
            match = '\.rs$'
            to = "a"
        "#;
        let cfg = load_from_str(text).unwrap();
        assert_eq!(first_match_idx(&cfg, "/tmp/README.md"), None);
    }

    #[test]
    fn array_match_is_or() {
        let text = r#"
            [todoke.a]
            command = "echo"

            [[rules]]
            match = ['\.rs$', '\.toml$']
            to = "a"
        "#;
        let cfg = load_from_str(text).unwrap();
        assert_eq!(first_match_idx(&cfg, "/tmp/x.rs"), Some(0));
        assert_eq!(first_match_idx(&cfg, "/tmp/x.toml"), Some(0));
        assert_eq!(first_match_idx(&cfg, "/tmp/x.md"), None);
    }

    #[test]
    fn exclude_single_pattern_skips_rule() {
        let text = r#"
            [todoke.a]
            command = "echo"

            [[rules]]
            name = "code"
            match = '.*'
            exclude = '\.md$'
            to = "a"

            [[rules]]
            name = "fallback"
            match = '.*'
            to = "a"
        "#;
        let cfg = load_from_str(text).unwrap();
        assert_eq!(first_match_idx(&cfg, "/x/foo.rs"), Some(0));
        assert_eq!(first_match_idx(&cfg, "/x/foo.md"), Some(1));
    }

    #[test]
    fn exclude_array_is_or() {
        let text = r#"
            [todoke.a]
            command = "echo"

            [[rules]]
            match = '.*'
            exclude = ['\.md$', '/tmp/']
            to = "a"
        "#;
        let cfg = load_from_str(text).unwrap();
        assert_eq!(first_match_idx(&cfg, "/x/foo.rs"), Some(0));
        assert_eq!(first_match_idx(&cfg, "/x/foo.md"), None);
        assert_eq!(first_match_idx(&cfg, "/tmp/foo.rs"), None);
    }

    #[test]
    fn url_subjects_match_as_is() {
        let text = r#"
            [todoke.firefox]
            command = "firefox"

            [[rules]]
            match = '^https?://github\.com/'
            to = "firefox"
        "#;
        let cfg = load_from_str(text).unwrap();
        assert_eq!(
            first_match_idx(&cfg, "https://github.com/owner/repo"),
            Some(0)
        );
        assert_eq!(first_match_idx(&cfg, "https://gitlab.com/owner/repo"), None);
    }

    #[test]
    fn default_config_matches_commit_editmsg() {
        let cfg = load_from_str(crate::config::DEFAULT_CONFIG_TOML).unwrap();
        let idx = first_match_idx(&cfg, "/home/x/repo/.git/COMMIT_EDITMSG");
        assert_eq!(idx, Some(0), "expected editor-callback rule to match");
        assert_eq!(
            cfg.rule(idx.unwrap()).name.as_deref(),
            Some("editor-callback")
        );

        let idx = first_match_idx(&cfg, "/home/x/notes/idea.md");
        assert_eq!(idx, Some(1));
        assert_eq!(cfg.rule(idx.unwrap()).name.as_deref(), Some("default"));
    }

    #[test]
    fn captures_numbered_groups() {
        let text = r#"
            [todoke.a]
            command = "echo"

            [[rules]]
            match = '^gh:([^/]+)/(.+)$'
            to = "a"
        "#;
        let cfg = load_from_str(text).unwrap();
        let (idx, caps) = first_match(&cfg, "gh:yukimemi/todoke", None).unwrap();
        assert_eq!(idx, 0);
        assert_eq!(
            caps.get("0").map(String::as_str),
            Some("gh:yukimemi/todoke")
        );
        assert_eq!(caps.get("1").map(String::as_str), Some("yukimemi"));
        assert_eq!(caps.get("2").map(String::as_str), Some("todoke"));
    }

    #[test]
    fn captures_named_groups() {
        let text = r#"
            [todoke.a]
            command = "echo"

            [[rules]]
            match = '^JIRA-(?P<id>\d+)$'
            to = "a"
        "#;
        let cfg = load_from_str(text).unwrap();
        let (_, caps) = first_match(&cfg, "JIRA-4321", None).unwrap();
        assert_eq!(caps.get("id").map(String::as_str), Some("4321"));
        // Numbered access still works alongside names.
        assert_eq!(caps.get("1").map(String::as_str), Some("4321"));
    }

    #[test]
    fn input_type_filter_skips_non_matching_kinds() {
        let text = r#"
            [todoke.a]
            command = "echo"

            [[rules]]
            name = "raw-only"
            match = '^HEAD$'
            to = "a"
            input_type = "raw"

            [[rules]]
            name = "fallback"
            match = '.*'
            to = "a"
        "#;
        let cfg = load_from_str(text).unwrap();
        // With kind=File, the raw-only rule is skipped and the catch-all wins.
        assert_eq!(
            first_match(&cfg, "HEAD", Some(InputKind::File)).map(|(i, _)| i),
            Some(1),
        );
        // With kind=Raw, the specific rule fires.
        assert_eq!(
            first_match(&cfg, "HEAD", Some(InputKind::Raw)).map(|(i, _)| i),
            Some(0),
        );
    }

    #[test]
    fn input_type_array_accepts_any_listed_kind() {
        let text = r#"
            [todoke.a]
            command = "echo"

            [[rules]]
            match = '.*'
            to = "a"
            input_type = ["file", "raw"]
        "#;
        let cfg = load_from_str(text).unwrap();
        assert_eq!(
            first_match(&cfg, "x", Some(InputKind::File)).map(|(i, _)| i),
            Some(0),
        );
        assert_eq!(
            first_match(&cfg, "x", Some(InputKind::Raw)).map(|(i, _)| i),
            Some(0),
        );
        assert!(first_match(&cfg, "x", Some(InputKind::Url)).is_none());
    }

    #[test]
    fn captures_empty_when_no_groups() {
        let text = r#"
            [todoke.a]
            command = "echo"

            [[rules]]
            match = '.*'
            to = "a"
        "#;
        let cfg = load_from_str(text).unwrap();
        let (_, caps) = first_match(&cfg, "anything", None).unwrap();
        // Full match still at "0".
        assert_eq!(caps.get("0").map(String::as_str), Some("anything"));
        // No other keys.
        assert_eq!(caps.len(), 1);
    }
}
