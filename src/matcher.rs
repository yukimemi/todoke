//! Regex-based rule matcher. First-match-wins across [[rules]].
//!
//! Patterns are pre-compiled in [`crate::config::ResolvedConfig::compile`] so
//! hot-path matching is cheap. A rule matches if ANY of its regexes hit the
//! normalized path.

use std::path::Path;

use crate::config::ResolvedConfig;

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

/// Find the first rule whose patterns match `normalized_path`. A rule counts
/// as matching when ANY of its `match` patterns hits AND NONE of its
/// `exclude` patterns hits. Returns the rule's index in
/// [`ResolvedConfig::raw::rules`].
pub fn first_match(cfg: &ResolvedConfig, normalized_path: &str) -> Option<usize> {
    for (i, regexes) in cfg.rule_regexes.iter().enumerate() {
        let matched = regexes.iter().any(|re| re.is_match(normalized_path));
        if !matched {
            continue;
        }
        let excluded = cfg.rule_excludes[i]
            .iter()
            .any(|re| re.is_match(normalized_path));
        if excluded {
            continue;
        }
        return Some(i);
    }
    None
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
        // \\?\UNC\server\share\file -> \\server\share\file (keep backslashes for nvim)
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
            [editors.a]
            kind = "generic"
            command = "echo"

            [[rules]]
            name = "rs"
            match = '\.rs$'
            editor = "a"

            [[rules]]
            name = "any"
            match = '.*'
            editor = "a"
        "#;
        let cfg = load_from_str(text).unwrap();
        assert_eq!(first_match(&cfg, "/tmp/foo.rs"), Some(0));
        assert_eq!(first_match(&cfg, "/tmp/README"), Some(1));
    }

    #[test]
    fn first_match_none_when_no_rule_fits() {
        let text = r#"
            [editors.a]
            kind = "generic"
            command = "echo"

            [[rules]]
            match = '\.rs$'
            editor = "a"
        "#;
        let cfg = load_from_str(text).unwrap();
        assert_eq!(first_match(&cfg, "/tmp/README.md"), None);
    }

    #[test]
    fn array_match_is_or() {
        let text = r#"
            [editors.a]
            kind = "generic"
            command = "echo"

            [[rules]]
            match = ['\.rs$', '\.toml$']
            editor = "a"
        "#;
        let cfg = load_from_str(text).unwrap();
        assert_eq!(first_match(&cfg, "/tmp/x.rs"), Some(0));
        assert_eq!(first_match(&cfg, "/tmp/x.toml"), Some(0));
        assert_eq!(first_match(&cfg, "/tmp/x.md"), None);
    }

    #[test]
    fn exclude_single_pattern_skips_rule() {
        let text = r#"
            [editors.a]
            kind = "generic"
            command = "echo"

            [[rules]]
            name = "code"
            match = '.*'
            exclude = '\.md$'
            editor = "a"

            [[rules]]
            name = "fallback"
            match = '.*'
            editor = "a"
        "#;
        let cfg = load_from_str(text).unwrap();
        // .rs file → first rule wins
        assert_eq!(first_match(&cfg, "/x/foo.rs"), Some(0));
        // .md file → first rule excluded, falls through
        assert_eq!(first_match(&cfg, "/x/foo.md"), Some(1));
    }

    #[test]
    fn exclude_array_is_or() {
        let text = r#"
            [editors.a]
            kind = "generic"
            command = "echo"

            [[rules]]
            match = '.*'
            exclude = ['\.md$', '/tmp/']
            editor = "a"
        "#;
        let cfg = load_from_str(text).unwrap();
        assert_eq!(first_match(&cfg, "/x/foo.rs"), Some(0));
        assert_eq!(first_match(&cfg, "/x/foo.md"), None);
        assert_eq!(first_match(&cfg, "/tmp/foo.rs"), None);
    }

    #[test]
    fn default_config_matches_commit_editmsg() {
        let cfg = load_from_str(crate::config::DEFAULT_CONFIG_TOML).unwrap();
        // git commit editor callback
        let idx = first_match(&cfg, "/home/x/repo/.git/COMMIT_EDITMSG");
        assert_eq!(idx, Some(0), "expected editor-callback rule to match");
        assert_eq!(
            cfg.rule(idx.unwrap()).name.as_deref(),
            Some("editor-callback")
        );

        // normal file falls through to the "default" rule
        let idx = first_match(&cfg, "/home/x/notes/idea.md");
        assert_eq!(idx, Some(1));
        assert_eq!(cfg.rule(idx.unwrap()).name.as_deref(), Some("default"));
    }
}
