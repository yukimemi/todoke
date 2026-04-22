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
            [todoke.echo]
            command = "echo"

            [[rules]]
            name = "default"
            match = '.*'
            to = "echo"
        "#,
    );

    let (ok, out, err) = run_with(&[
        "--todoke-config",
        &config.to_string_lossy(),
        "--todoke-dry-run",
    ]);
    assert!(ok, "stderr: {err}");
    assert!(out.contains("to=echo"), "stdout: {out}");
    assert!(out.contains("rule=default"), "stdout: {out}");
}

#[test]
fn gnu_posix_argument_syntax_parses_end_to_end() {
    // Covers the major shapes from POSIX Utility Syntax Guidelines +
    // GNU Argument Syntax:
    //   - short flag `-f`                (single flag)
    //   - long flag `--flag`             (single flag)
    //   - long w/ `=value` `--key=val`   (one argv)
    //   - short concatenated `-sfoo`     (one argv, POSIX option-arg)
    //   - bundled shorts `-abc`          (one argv)
    //   - vim/ed-style `+N`              (POSIX ex-editor convention)
    //   - short spaced `-s val`          (two argv, consumes = 1)
    //   - long spaced `--long val`       (two argv, consumes = 1)
    //   - variadic `-p a b c`            (consumes_until)
    //   - positional file                (file input)
    //   - stdin marker `-`               (passthrough)
    //
    // The rule set uses match regexes anchored to full argv, so every
    // pattern lands in exactly one rule.
    //
    // GNU separator `--` is not exercised here because clap consumes it
    // as the end-of-options marker before todoke's argv parser sees it —
    // `consumes_rest` is covered by a dedicated rule pattern below.

    let dir = temp_dir();
    let config = dir.join("todoke.toml");
    write_file(
        &config,
        r#"
            [todoke.echo]
            command = "echo"

            # POSIX short flag with spaced value (fixed 1)
            [[rules]]
            name = "short-spaced"
            match = '^-s$'
            to = "echo"
            passthrough = true
            consumes = 1

            # GNU long flag with spaced value (fixed 1)
            [[rules]]
            name = "long-spaced"
            match = '^--long$'
            to = "echo"
            passthrough = true
            consumes = 1

            # Variadic flag: consume argv until the next flag
            [[rules]]
            name = "variadic"
            match = '^-p$'
            to = "echo"
            passthrough = true
            consumes_until = '^[-+]'

            # `++rest` sentinel: after this token, eat every remaining argv.
            # Uses a non-`--` sentinel because clap swallows `--` before it
            # reaches todoke's own argv parser.
            [[rules]]
            name = "rest-sentinel"
            match = '^\+\+rest$'
            to = "echo"
            passthrough = true
            consumes_rest = true

            # Any other flag-shaped argv (short/long/bundled/concatenated/plus/stdin-dash)
            [[rules]]
            name = "any-flag"
            match = '^[-+]'
            to = "echo"
            passthrough = true

            # Default: positional files
            [[rules]]
            name = "default"
            match = '.*'
            to = "echo"
        "#,
    );

    let cfg_str = config.to_string_lossy().into_owned();

    // No `--` separator: the whole point of `allow_hyphen_values` on the
    // positional is that hyphen-shaped argv (`-f`, `+42`, `-sfoo`, …)
    // flow through as positionals without escape.
    let run_dry = |extra: &[&str]| -> String {
        let mut args: Vec<&str> = vec!["--todoke-config", &cfg_str, "--todoke-dry-run"];
        args.extend_from_slice(extra);
        let (ok, out, err) = run_with(&args);
        assert!(ok, "failed for {extra:?}: stderr: {err}");
        out
    };

    struct Case<'a> {
        label: &'a str,
        args: &'a [&'a str],
        expect_passthrough: &'a [&'a str],
        expect_file_contains: &'a [&'a str],
    }

    let cases = [
        Case {
            label: "POSIX short flag",
            args: &["-f"],
            expect_passthrough: &["-f"],
            expect_file_contains: &[],
        },
        Case {
            label: "GNU long flag",
            args: &["--flag"],
            expect_passthrough: &["--flag"],
            expect_file_contains: &[],
        },
        Case {
            label: "GNU long=value",
            args: &["--key=val"],
            expect_passthrough: &["--key=val"],
            expect_file_contains: &[],
        },
        Case {
            label: "POSIX short concatenated (-sfoo)",
            args: &["-sfoo"],
            expect_passthrough: &["-sfoo"],
            expect_file_contains: &[],
        },
        Case {
            label: "POSIX bundled shorts (-abc)",
            args: &["-abc"],
            expect_passthrough: &["-abc"],
            expect_file_contains: &[],
        },
        Case {
            label: "ex-editor plus flag (+42)",
            args: &["+42"],
            expect_passthrough: &["+42"],
            expect_file_contains: &[],
        },
        Case {
            label: "POSIX short spaced (-s val)",
            args: &["-s", "val"],
            expect_passthrough: &["-s", "val"],
            expect_file_contains: &[],
        },
        Case {
            label: "GNU long spaced (--long val)",
            args: &["--long", "val"],
            expect_passthrough: &["--long", "val"],
            expect_file_contains: &[],
        },
        Case {
            label: "variadic (-p a b c)",
            args: &["-p", "a", "b", "c"],
            expect_passthrough: &["-p", "a", "b", "c"],
            expect_file_contains: &[],
        },
        Case {
            label: "rest sentinel (++rest x y)",
            args: &["++rest", "x", "y"],
            expect_passthrough: &["++rest", "x", "y"],
            expect_file_contains: &[],
        },
        Case {
            label: "positional file",
            args: &["some-nonexistent-file.txt"],
            expect_passthrough: &[],
            expect_file_contains: &["some-nonexistent-file.txt"],
        },
        Case {
            label: "stdin marker (-)",
            args: &["-"],
            expect_passthrough: &["-"],
            expect_file_contains: &[],
        },
        Case {
            label: "mixed (-s val -p a b ++rest z)",
            args: &["-s", "val", "-p", "a", "b", "++rest", "z"],
            expect_passthrough: &["-s", "val", "-p", "a", "b", "++rest", "z"],
            expect_file_contains: &[],
        },
    ];

    for case in &cases {
        let out = run_dry(case.args);
        for p in case.expect_passthrough {
            let needle = format!("[passthrough] {p}");
            assert!(
                out.contains(&needle),
                "{}: expected {needle:?} in plan\n--- plan ---\n{out}",
                case.label,
            );
        }
        for f in case.expect_file_contains {
            assert!(
                out.contains("[file]") && out.contains(f),
                "{}: expected [file] + {f:?} in plan\n--- plan ---\n{out}",
                case.label,
            );
        }
    }
}

#[test]
fn no_args_no_rules_errors() {
    let dir = temp_dir();
    let config = dir.join("todoke.toml");
    write_file(
        &config,
        r#"
            [todoke.echo]
            command = "echo"
        "#,
    );

    let (ok, _out, err) = run_with(&[
        "--todoke-config",
        &config.to_string_lossy(),
        "--todoke-dry-run",
    ]);
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
            [todoke.echo]
            command = "echo"

            [[rules]]
            name = "default"
            match = '.*'
            to = "echo"
            group = "default"
            mode = "remote"
            sync = false
        "#,
    );

    let (ok, out, err) = run_with(&[
        "--todoke-config",
        &config.to_string_lossy(),
        "--todoke-dry-run",
        &file.to_string_lossy(),
    ]);
    assert!(ok, "stderr: {err}");
    assert!(out.contains("to=echo"), "stdout: {out}");
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
            [todoke.echo]
            command = "echo"

            [[rules]]
            name = "rust"
            match = '\.rs$'
            to = "echo"
            group = "code"

            [[rules]]
            name = "fallback"
            match = '.*'
            to = "echo"
        "#,
    );

    let (ok, out, err) = run_with(&[
        "--todoke-config",
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
        "--todoke-config",
        &config.to_string_lossy(),
        "--todoke-dry-run",
        &file.to_string_lossy(),
    ]);
    assert!(!ok);
    assert!(err.contains("failed to parse TOML"), "stderr: {err}");
}

#[test]
fn unknown_to_reference_reports_error() {
    let dir = temp_dir();
    let file = dir.join("x.txt");
    write_file(&file, "x");
    let config = dir.join("bad.toml");
    write_file(
        &config,
        r#"
            [[rules]]
            match = ".*"
            to = "does-not-exist"
        "#,
    );

    let (ok, _out, err) = run_with(&[
        "--todoke-config",
        &config.to_string_lossy(),
        "--todoke-dry-run",
        &file.to_string_lossy(),
    ]);
    assert!(!ok);
    assert!(err.contains("unknown todoke target"), "stderr: {err}");
}
