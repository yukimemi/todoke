//! Tera wrapper.
//!
//! - Registers todoke-specific functions: `is_windows`, `is_linux`, `is_mac`.
//! - Builds a per-dispatch [`tera::Context`] populated with `input`,
//!   `input_type`, `file_*` (for file inputs), `url_*` (for URL inputs),
//!   `command_*`, `cwd`, `group`, `rule`, `vars.*`, `env.*`.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use anyhow::Result;
use tera::{Function, Tera, Value};

use crate::input::Input;
use crate::platform;

/// Build a fresh Tera engine with todoke's custom OS functions registered.
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

/// Inputs to [`build_context`]. Holds references so callers don't have to
/// allocate.
pub struct Context<'a> {
    pub input: Option<&'a Input>,
    pub command: &'a str,
    pub cwd: &'a str,
    pub group: &'a str,
    pub rule_name: &'a str,
    pub vars: &'a BTreeMap<String, toml::Value>,
    /// Capture groups from the matched rule's regex; keyed by index
    /// (`"0"`, `"1"`, …) and by name. Empty when no capture / no match.
    pub cap: &'a crate::matcher::CaptureMap,
}

fn strip_verbatim_str(s: &str) -> String {
    s.strip_prefix(r"\\?\").unwrap_or(s).to_string()
}

fn file_vars(p: &Path) -> [(String, String); 5] {
    let full = strip_verbatim_str(&p.to_string_lossy());
    let dir = p
        .parent()
        .map(|x| strip_verbatim_str(&x.to_string_lossy()))
        .unwrap_or_default();
    let name = p
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stem = p
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = p
        .extension()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    [
        ("file_path".into(), full),
        ("file_dir".into(), dir),
        ("file_name".into(), name),
        ("file_stem".into(), stem),
        ("file_ext".into(), ext),
    ]
}

fn url_vars(u: &url::Url) -> [(String, String); 6] {
    [
        ("url_scheme".into(), u.scheme().to_string()),
        ("url_host".into(), u.host_str().unwrap_or("").to_string()),
        (
            "url_port".into(),
            u.port().map(|p| p.to_string()).unwrap_or_default(),
        ),
        ("url_path".into(), u.path().to_string()),
        ("url_query".into(), u.query().unwrap_or("").to_string()),
        (
            "url_fragment".into(),
            u.fragment().unwrap_or("").to_string(),
        ),
    ]
}

fn command_vars(command: &str) -> [(String, String); 5] {
    let p = Path::new(command);
    let [(_, path), (_, dir), (_, name), (_, stem), (_, ext)] = file_vars(p);
    [
        ("command_path".into(), path),
        ("command_dir".into(), dir),
        ("command_name".into(), name),
        ("command_stem".into(), stem),
        ("command_ext".into(), ext),
    ]
}

const EMPTY_FILE_KEYS: [&str; 5] = [
    "file_path",
    "file_dir",
    "file_name",
    "file_stem",
    "file_ext",
];
const EMPTY_URL_KEYS: [&str; 6] = [
    "url_scheme",
    "url_host",
    "url_port",
    "url_path",
    "url_query",
    "url_fragment",
];

/// Build a Tera context for a dispatch. When the input is `None` (e.g. the
/// no-args invocation), all input-derived variables are empty strings.
pub fn build_context(c: Context<'_>) -> tera::Context {
    let mut ctx = tera::Context::new();

    // Universal input vars
    let (input_str, input_type) = match c.input {
        Some(i) => (i.display_string(), i.kind_label()),
        None => (String::new(), ""),
    };
    ctx.insert("input", &input_str);
    ctx.insert("input_type", input_type);

    // Populate file_* and url_* based on input type; the unused half gets
    // empty strings so `{{ file_path }}` never fails in strict Tera.
    match c.input {
        Some(Input::File(p)) => {
            for (k, v) in file_vars(p) {
                ctx.insert(&k, &v);
            }
            for k in EMPTY_URL_KEYS {
                ctx.insert(k, "");
            }
        }
        Some(Input::Url(u)) => {
            for k in EMPTY_FILE_KEYS {
                ctx.insert(k, "");
            }
            for (k, v) in url_vars(u) {
                ctx.insert(&k, &v);
            }
        }
        Some(Input::Raw(_)) | None => {
            for k in EMPTY_FILE_KEYS {
                ctx.insert(k, "");
            }
            for k in EMPTY_URL_KEYS {
                ctx.insert(k, "");
            }
        }
    }

    for (k, v) in command_vars(c.command) {
        ctx.insert(&k, &v);
    }

    ctx.insert("cwd", c.cwd);
    ctx.insert("group", c.group);
    ctx.insert("rule", c.rule_name);

    let env_map: HashMap<String, String> = std::env::vars().collect();
    ctx.insert("env", &env_map);

    let vars_map: HashMap<String, toml::Value> = c.vars.clone().into_iter().collect();
    ctx.insert("vars", &vars_map);

    // cap.<N> / cap.<name>. Empty map when no capture groups matched.
    let cap_map: HashMap<String, String> = c.cap.clone().into_iter().collect();
    ctx.insert("cap", &cap_map);

    ctx
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn build(input: Option<&Input>, vars: &BTreeMap<String, toml::Value>) -> tera::Context {
        let cap = BTreeMap::new();
        build_context(Context {
            input,
            command: "",
            cwd: "/cwd",
            group: "",
            rule_name: "",
            vars,
            cap: &cap,
        })
    }

    #[test]
    fn file_input_populates_file_vars() {
        let i = Input::File(PathBuf::from("/tmp/hello.rs"));
        let ctx = build(Some(&i), &BTreeMap::new());
        let mut tera = new_engine();
        let out = render(
            &mut tera,
            "{{ file_stem }}.{{ file_ext }} / {{ input_type }}",
            &ctx,
        )
        .unwrap();
        assert_eq!(out, "hello.rs / file");
    }

    #[test]
    fn url_input_populates_url_vars() {
        let i =
            Input::Url(url::Url::parse("https://github.com/yukimemi/todoke?tab=rs#top").unwrap());
        let ctx = build(Some(&i), &BTreeMap::new());
        let mut tera = new_engine();
        let out = render(
            &mut tera,
            "{{ url_scheme }}://{{ url_host }}{{ url_path }}?{{ url_query }}#{{ url_fragment }} / {{ input_type }}",
            &ctx,
        )
        .unwrap();
        assert_eq!(out, "https://github.com/yukimemi/todoke?tab=rs#top / url");
    }

    #[test]
    fn file_keys_empty_for_url_inputs() {
        let i = Input::Url(url::Url::parse("https://example.com").unwrap());
        let ctx = build(Some(&i), &BTreeMap::new());
        let mut tera = new_engine();
        let out = render(&mut tera, "[{{ file_path }}]", &ctx).unwrap();
        assert_eq!(out, "[]");
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
    fn cap_map_exposed_in_template() {
        let mut cap = BTreeMap::new();
        cap.insert("0".into(), "issue:42".into());
        cap.insert("1".into(), "42".into());
        cap.insert("id".into(), "42".into());
        let ctx = build_context(Context {
            input: None,
            command: "",
            cwd: "/cwd",
            group: "",
            rule_name: "",
            vars: &BTreeMap::new(),
            cap: &cap,
        });
        let mut tera = new_engine();
        let out = render(&mut tera, "{{ cap.0 }} / {{ cap.1 }} / {{ cap.id }}", &ctx).unwrap();
        assert_eq!(out, "issue:42 / 42 / 42");
    }

    #[test]
    fn vars_and_env_substitute() {
        unsafe { std::env::set_var("TODOKE_TEST_VAR", "test_value") };
        let mut vars = BTreeMap::new();
        vars.insert("greeting".into(), toml::Value::String("hi".into()));
        let ctx = build(None, &vars);
        let mut tera = new_engine();
        let out = render(
            &mut tera,
            "{{ vars.greeting }} {{ env.TODOKE_TEST_VAR }}",
            &ctx,
        )
        .unwrap();
        assert_eq!(out, "hi test_value");
    }
}
