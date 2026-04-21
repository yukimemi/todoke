//! Integration tests for the `todoke` CLI.
//!
//! Uses `cargo run` via env!("CARGO_BIN_EXE_todoke") so no extra dependencies
//! are needed beyond the standard library and tempfile.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_todoke"))
}

fn write_file(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let mut f = std::fs::File::create(path).unwrap();
    f.write_all(contents.as_bytes()).unwrap();
}

fn temp_dir() -> PathBuf {
    let base = std::env::temp_dir().join("todoke-test");
    // unique-ish per test via nano timestamp
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let d = base.join(stamp.to_string());
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn run_with(args: &[&str]) -> (bool, String, String) {
    let out = Command::new(bin())
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    (out.status.success(), stdout, stderr)
}

#[test]
fn help_succeeds() {
    let (ok, out, _) = run_with(&["--help"]);
    assert!(ok);
    assert!(out.contains("todoke"));
    assert!(out.contains("dispatch"));
}

#[test]
fn version_succeeds() {
    let (ok, out, _) = run_with(&["--version"]);
    assert!(ok);
    assert!(out.contains("todoke"));
}

#[test]
fn no_args_uses_default_rule() {
    let dir = temp_dir();
    let config = dir.join("todoke.toml");
    write_file(
        &config,
        r#"
            [editors.echo]
            kind = "generic"
            command = "echo"

            [[rules]]
            name = "default"
            match = '.*'
            editor = "echo"
        "#,
    );

    let (ok, out, err) = run_with(&["--config", &config.to_string_lossy(), "--dry-run"]);
    assert!(ok, "stderr: {err}");
    assert!(out.contains("editor=echo"), "stdout: {out}");
    assert!(out.contains("rule=default"), "stdout: {out}");
}

#[test]
fn no_args_no_rules_errors() {
    let dir = temp_dir();
    let config = dir.join("todoke.toml");
    write_file(
        &config,
        r#"
            [editors.echo]
            kind = "generic"
            command = "echo"
        "#,
    );

    let (ok, _out, err) = run_with(&["--config", &config.to_string_lossy(), "--dry-run"]);
    assert!(!ok);
    assert!(err.contains("no rule matches empty-args"), "stderr: {err}");
}

#[test]
fn kill_without_args_errors() {
    let (ok, _stdout, stderr) = run_with(&["kill"]);
    assert!(!ok);
    assert!(
        stderr.contains("specify <group> or --all"),
        "stderr: {stderr}"
    );
}

#[test]
fn dry_run_plans_default_rule() {
    let dir = temp_dir();
    let file = dir.join("note.md");
    write_file(&file, "hello\n");

    let config = dir.join("todoke.toml");
    write_file(
        &config,
        r#"
            [editors.echo]
            kind = "generic"
            command = "echo"

            [[rules]]
            name = "default"
            match = '.*'
            editor = "echo"
            group = "default"
            mode = "remote"
            sync = false
        "#,
    );

    let (ok, out, err) = run_with(&[
        "--config",
        &config.to_string_lossy(),
        "--dry-run",
        &file.to_string_lossy(),
    ]);
    assert!(ok, "stderr: {err}");
    assert!(out.contains("editor=echo"), "stdout: {out}");
    assert!(out.contains("group=default"), "stdout: {out}");
    assert!(out.contains("rule=default"), "stdout: {out}");
}

#[test]
fn check_shows_matched_rule_per_file() {
    let dir = temp_dir();
    let rs_file = dir.join("main.rs");
    let md_file = dir.join("note.md");
    write_file(&rs_file, "fn main(){}\n");
    write_file(&md_file, "# hi\n");

    let config = dir.join("todoke.toml");
    write_file(
        &config,
        r#"
            [editors.echo]
            kind = "generic"
            command = "echo"

            [[rules]]
            name = "rust"
            match = '\.rs$'
            editor = "echo"
            group = "code"

            [[rules]]
            name = "fallback"
            match = '.*'
            editor = "echo"
        "#,
    );

    let (ok, out, err) = run_with(&[
        "--config",
        &config.to_string_lossy(),
        "check",
        &rs_file.to_string_lossy(),
        &md_file.to_string_lossy(),
    ]);
    assert!(ok, "stderr: {err}");
    assert!(out.contains("rule=rust"), "stdout: {out}");
    assert!(out.contains("group=code"), "stdout: {out}");
    assert!(out.contains("rule=fallback"), "stdout: {out}");
}

#[test]
fn completion_bash_outputs_script() {
    let (ok, out, _err) = run_with(&["completion", "bash"]);
    assert!(ok);
    assert!(
        out.contains("_todoke()"),
        "stdout head: {}",
        &out[..out.len().min(200)]
    );
}

#[test]
fn invalid_config_reports_error() {
    let dir = temp_dir();
    let file = dir.join("x.txt");
    write_file(&file, "x");
    let config = dir.join("bad.toml");
    write_file(&config, "this is not valid toml [[[");

    let (ok, _out, err) = run_with(&[
        "--config",
        &config.to_string_lossy(),
        "--dry-run",
        &file.to_string_lossy(),
    ]);
    assert!(!ok);
    assert!(err.contains("failed to parse TOML"), "stderr: {err}");
}

#[test]
fn unknown_editor_reference_reports_error() {
    let dir = temp_dir();
    let file = dir.join("x.txt");
    write_file(&file, "x");
    let config = dir.join("bad.toml");
    write_file(
        &config,
        r#"
            [[rules]]
            match = ".*"
            editor = "does-not-exist"
        "#,
    );

    let (ok, _out, err) = run_with(&[
        "--config",
        &config.to_string_lossy(),
        "--dry-run",
        &file.to_string_lossy(),
    ]);
    assert!(!ok);
    assert!(err.contains("unknown editor"), "stderr: {err}");
}
