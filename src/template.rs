//! Tera wrapper.
//!
//! - Registers edtr-specific functions: `is_windows`, `is_linux`, `is_mac`.
//! - Builds a per-dispatch [`tera::Context`] populated with `file_*`,
//!   `editor_*`, `cwd`, `group`, `rule`, `vars.*`, `env.*` as established in
//!   the design phase.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use tera::{Function, Tera, Value};

use crate::platform;

/// Build a fresh Tera engine with edtr's custom OS functions registered.
pub fn new_engine() -> Tera {
    let mut t = Tera::default();
    t.register_function("is_windows", os_fn(platform::is_windows));
    t.register_function("is_linux", os_fn(platform::is_linux));
    t.register_function("is_mac", os_fn(platform::is_mac));
    t
}

fn os_fn(check: fn() -> bool) -> impl Function {
    move |_args: &HashMap<String, Value>| -> tera::Result<Value> { Ok(Value::Bool(check())) }
}

/// Render a template string with the given context. Returns an owned String.
pub fn render(tera: &mut Tera, template: &str, ctx: &tera::Context) -> Result<String> {
    Ok(tera.render_str(template, ctx)?)
}

/// Per-file parts of the template context.
#[derive(Debug, Clone)]
pub struct FileParts {
    pub path: PathBuf,
    pub dir: String,
    pub name: String,
    pub stem: String,
    pub ext: String,
}

impl FileParts {
    pub fn from_path(p: &Path) -> Self {
        let path = p.to_path_buf();
        let dir = p.parent().map(path_string).unwrap_or_default();
        let name = p
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let stem = p
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let ext = p
            .extension()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        Self {
            path,
            dir,
            name,
            stem,
            ext,
        }
    }
}

fn path_string(p: &Path) -> String {
    // strip Windows `\\?\` verbatim prefix; keep backslashes intact — callers
    // who want forward slashes should apply a Tera filter explicitly.
    let s = p.to_string_lossy();
    s.strip_prefix(r"\\?\").unwrap_or(&s).to_string()
}

/// Build a Tera context for a dispatch. `group` and `rule` may be empty
/// strings when rendering phase-2 templates (rule.group itself); the caller
/// supplies them for phase-3 (editor templates).
pub fn build_context(
    file: &FileParts,
    editor_cmd_parts: Option<&FileParts>,
    cwd: &str,
    group: &str,
    rule_name: &str,
    vars: &std::collections::BTreeMap<String, toml::Value>,
) -> tera::Context {
    let mut ctx = tera::Context::new();

    ctx.insert("file_path", &path_string(&file.path));
    ctx.insert("file_dir", &file.dir);
    ctx.insert("file_name", &file.name);
    ctx.insert("file_stem", &file.stem);
    ctx.insert("file_ext", &file.ext);

    if let Some(ed) = editor_cmd_parts {
        ctx.insert("editor_path", &path_string(&ed.path));
        ctx.insert("editor_dir", &ed.dir);
        ctx.insert("editor_name", &ed.name);
        ctx.insert("editor_stem", &ed.stem);
        ctx.insert("editor_ext", &ed.ext);
    } else {
        // Placeholders so templates that reference these don't blow up during
        // phase-2 (rule.group) rendering.
        ctx.insert("editor_path", "");
        ctx.insert("editor_dir", "");
        ctx.insert("editor_name", "");
        ctx.insert("editor_stem", "");
        ctx.insert("editor_ext", "");
    }

    ctx.insert("cwd", cwd);
    ctx.insert("group", group);
    ctx.insert("rule", rule_name);

    let env_map: HashMap<String, String> = std::env::vars().collect();
    ctx.insert("env", &env_map);

    let vars_map: HashMap<String, toml::Value> = vars.clone().into_iter().collect();
    ctx.insert("vars", &vars_map);

    ctx
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    #[test]
    fn file_parts_rust_path_semantics() {
        let p = PathBuf::from("/tmp/foo.rs");
        let fp = FileParts::from_path(&p);
        assert_eq!(fp.name, "foo.rs");
        assert_eq!(fp.stem, "foo");
        assert_eq!(fp.ext, "rs"); // no leading dot
    }

    #[test]
    fn file_parts_handles_double_extension() {
        let p = PathBuf::from("/tmp/foo.tar.gz");
        let fp = FileParts::from_path(&p);
        assert_eq!(fp.stem, "foo.tar"); // Rust's Path::file_stem strips last ext
        assert_eq!(fp.ext, "gz");
    }

    #[test]
    fn file_parts_handles_no_extension() {
        let p = PathBuf::from("/tmp/Makefile");
        let fp = FileParts::from_path(&p);
        assert_eq!(fp.name, "Makefile");
        assert_eq!(fp.stem, "Makefile");
        assert_eq!(fp.ext, ""); // empty, not "Makefile"
    }

    #[test]
    fn os_function_returns_correct_bool() {
        let mut tera = new_engine();
        let ctx = tera::Context::new();
        let rendered = tera
            .render_str("{% if is_windows() %}W{% else %}nW{% endif %}", &ctx)
            .unwrap();
        if cfg!(target_os = "windows") {
            assert_eq!(rendered, "W");
        } else {
            assert_eq!(rendered, "nW");
        }
    }

    #[test]
    fn renders_file_path_and_vars() {
        let file = FileParts::from_path(Path::new("/tmp/hello.md"));
        let mut vars = BTreeMap::new();
        vars.insert("greeting".into(), toml::Value::String("hi".into()));
        let ctx = build_context(&file, None, "/cwd", "default", "default", &vars);
        let mut tera = new_engine();
        let out = render(
            &mut tera,
            "{{ file_stem }}/{{ file_ext }} -> {{ vars.greeting }}",
            &ctx,
        )
        .unwrap();
        assert_eq!(out, "hello/md -> hi");
    }

    #[test]
    fn renders_env_var() {
        unsafe { std::env::set_var("EDTR_TEST_VAR", "test_value") };
        let file = FileParts::from_path(Path::new("/tmp/x"));
        let ctx = build_context(&file, None, "/cwd", "g", "r", &BTreeMap::new());
        let mut tera = new_engine();
        let out = render(&mut tera, "{{ env.EDTR_TEST_VAR }}", &ctx).unwrap();
        assert_eq!(out, "test_value");
    }
}
