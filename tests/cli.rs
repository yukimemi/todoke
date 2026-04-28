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
    // Windows' SystemTime has ~100ns resolution, and cargo test runs
    // integration tests in parallel. Nanos alone collided on CI and let
    // one test's config overwrite another's. Atomic counter + pid makes
    // it deterministically unique without needing a uuid dep.
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let base = std::env::temp_dir().join("todoke-test");
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let pid = std::process::id();
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let d = base.join(format!("{stamp}-{pid}-{seq}"));
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

    let (ok, out, err) = run_with(&["--todoke-config", &config.to_string_lossy(), "check"]);
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
        let mut args: Vec<&str> = vec!["--todoke-config", &cfg_str, "check"];
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

    let (ok, _out, err) = run_with(&["--todoke-config", &config.to_string_lossy(), "check"]);
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
        "check",
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
        "check",
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
        "check",
        &file.to_string_lossy(),
    ]);
    assert!(!ok);
    assert!(err.contains("unknown todoke target"), "stderr: {err}");
}

// --- `config` subcommand ---

#[test]
fn config_path_prints_explicit_override() {
    let dir = temp_dir();
    let config = dir.join("custom.toml");
    let (ok, out, err) = run_with(&[
        "--todoke-config",
        &config.to_string_lossy(),
        "config",
        "path",
    ]);
    assert!(ok, "stderr: {err}");
    assert!(out.trim_end().ends_with("custom.toml"), "stdout: {out}");
}

#[test]
fn config_show_prints_embedded_default_when_no_file() {
    let dir = temp_dir();
    let config = dir.join("missing.toml");
    let (ok, out, err) = run_with(&[
        "--todoke-config",
        &config.to_string_lossy(),
        "config",
        "show",
    ]);
    assert!(ok, "stderr: {err}");
    // sanity-check anchors from the bundled default.toml
    assert!(out.contains("[todoke.nvim]"), "stdout: {out}");
    assert!(out.contains("editor-callback"), "stdout: {out}");
}

#[test]
fn config_show_prints_user_file_contents() {
    let dir = temp_dir();
    let config = dir.join("todoke.toml");
    let body = "# my custom config\n[todoke.code]\ncommand = \"code\"\n";
    write_file(&config, body);
    let (ok, out, err) = run_with(&[
        "--todoke-config",
        &config.to_string_lossy(),
        "config",
        "show",
    ]);
    assert!(ok, "stderr: {err}");
    assert!(out.contains("# my custom config"), "stdout: {out}");
    assert!(out.contains("[todoke.code]"), "stdout: {out}");
}

#[test]
fn config_show_rendered_runs_tera() {
    let dir = temp_dir();
    let config = dir.join("todoke.toml");
    write_file(
        &config,
        r#"
            [vars]
            gui = "neovide"

            [todoke.gui]
            command = "{{ vars.gui }}"

            [[rules]]
            match = ".*"
            to = "gui"
        "#,
    );
    let (ok, out, err) = run_with(&[
        "--todoke-config",
        &config.to_string_lossy(),
        "config",
        "show",
        "--rendered",
    ]);
    assert!(ok, "stderr: {err}");
    assert!(
        out.contains("command = \"neovide\""),
        "expected rendered Tera output, got: {out}"
    );
    // The pre-rendered form must not survive --rendered.
    assert!(
        !out.contains("{{ vars.gui }}"),
        "stdout still has raw Tera tag: {out}"
    );
}

#[test]
fn config_init_writes_default_when_missing() {
    let dir = temp_dir();
    let config = dir.join("subdir").join("todoke.toml");
    assert!(!config.exists());

    let (ok, out, err) = run_with(&[
        "--todoke-config",
        &config.to_string_lossy(),
        "config",
        "init",
    ]);
    assert!(ok, "stderr: {err}");
    assert!(config.exists(), "config file should have been created");
    let written = std::fs::read_to_string(&config).unwrap();
    assert!(written.contains("[todoke.nvim]"), "wrote: {written}");
    assert!(out.contains("todoke.toml"), "stdout: {out}");
    assert!(
        err.contains("wrote default config"),
        "stderr should announce the write: {err}"
    );
}

#[test]
fn config_init_is_idempotent() {
    let dir = temp_dir();
    let config = dir.join("todoke.toml");
    write_file(&config, "# preserved by user\n");

    let (ok, _out, err) = run_with(&[
        "--todoke-config",
        &config.to_string_lossy(),
        "config",
        "init",
    ]);
    assert!(ok, "stderr: {err}");
    let after = std::fs::read_to_string(&config).unwrap();
    assert_eq!(after, "# preserved by user\n");
    assert!(
        !err.contains("wrote default config"),
        "should not announce a write when the file already exists: {err}"
    );
}

// --- `doctor` subcommand ---

#[test]
fn doctor_succeeds_for_user_file() {
    let dir = temp_dir();
    let config = dir.join("todoke.toml");
    write_file(
        &config,
        r#"
            [todoke.echo]
            command = "echo"

            [[rules]]
            name = "default"
            match = ".*"
            to = "echo"
        "#,
    );
    let (ok, out, err) = run_with(&["--todoke-config", &config.to_string_lossy(), "doctor"]);
    assert!(ok, "stderr: {err}");
    assert!(out.contains("no issues"), "stdout: {out}");
}

#[test]
fn doctor_fails_for_broken_toml() {
    let dir = temp_dir();
    let config = dir.join("bad.toml");
    write_file(&config, "this is not valid toml [[[");
    let (ok, _out, err) = run_with(&["--todoke-config", &config.to_string_lossy(), "doctor"]);
    assert!(!ok);
    assert!(err.contains("failed to parse TOML"), "stderr: {err}");
}

// --- neovim backend: spawn_detached_with_listen ---

/// When no nvim is listening yet, todoke exec()'s into `nvim --listen <socket>`
/// on Unix so nvim inherits the terminal (hitori.vim singleton behaviour).
/// In the test we have no TTY, so we pass `--headless` via args.remote.
/// todoke's process *becomes* nvim (exec) — there is no separate todoke exit.
/// We poll until the socket appears (nvim binds it on startup).
#[cfg(unix)]
#[test]
fn spawned_nvim_listen_binds_socket() {
    use std::os::unix::net::UnixStream;

    // Skip gracefully when nvim is not available in this environment.
    if !Command::new("nvim")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
    {
        eprintln!("skipping spawned_nvim_listen_binds_socket: nvim not available");
        return;
    }

    let dir = temp_dir();
    let socket = dir.join("test.sock");
    let config = dir.join("todoke.toml");
    write_file(
        &config,
        &format!(
            r#"
[todoke.nvim]
kind = "neovim"
command = "nvim"
listen = "{sock}"

[todoke.nvim.args]
remote = ["--headless"]

[[rules]]
name = "default"
match = ".*"
to = "nvim"
group = "default"
mode = "remote"
sync = false
"#,
            sock = socket.display()
        ),
    );

    // todoke exec()'s into nvim, so this child handle refers to nvim itself.
    // Capture stderr so assertion failures include nvim's diagnostic output.
    let mut child = Command::new(bin())
        .args(["--todoke-config", &config.to_string_lossy()])
        .args(["--", "somefile.txt"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    // Poll until the socket appears (nvim binds it on startup).
    // Timeout is configurable via TODOKE_TEST_TIMEOUT_MS (default 15 s) to
    // avoid flakes on slow/loaded CI while keeping the default sensible.
    let timeout_ms = std::env::var("TODOKE_TEST_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(15_000);
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    let conn = loop {
        if let Ok(c) = UnixStream::connect(&socket) {
            break Ok(c);
        }
        if std::time::Instant::now() > deadline {
            break Err(());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    };

    // Kill nvim (the exec'd process) and collect stderr before asserting.
    child.kill().ok();
    let stderr_bytes = child
        .wait_with_output()
        .map(|o| o.stderr)
        .unwrap_or_default();
    let stderr = String::from_utf8_lossy(&stderr_bytes);

    assert!(
        conn.is_ok(),
        "nvim exited after todoke returned — socket not connectable\nstderr: {stderr}"
    );
}

// --- list / kill ----------------------------------------------------
//
// Discovery walks the filesystem for the listen pattern; we don't spawn
// a real nvim here, so any matched candidates surface as `stale`. That
// is enough to exercise the CLI plumbing end-to-end. The
// `#[cfg(unix)]` guard isolates these tests from Windows' named-pipe
// enumeration, which can't be staged with plain `File::create`.

#[test]
fn list_reports_no_instances_for_exec_only_config() {
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
    let (ok, out, err) = run_with(&["--todoke-config", &config.to_string_lossy(), "list"]);
    assert!(ok, "stderr: {err}");
    assert!(out.contains("no instances found"), "stdout: {out}");
}

#[test]
fn kill_all_with_no_instances_succeeds_with_info() {
    let dir = temp_dir();
    let config = dir.join("todoke.toml");
    write_file(
        &config,
        r#"
            [todoke.echo]
            command = "echo"

            [[rules]]
            match = '.*'
            to = "echo"
        "#,
    );
    let (ok, out, err) = run_with(&[
        "--todoke-config",
        &config.to_string_lossy(),
        "kill",
        "--all",
    ]);
    assert!(ok, "stderr: {err}");
    assert!(out.contains("no matching instance"), "stdout: {out}");
}

#[cfg(unix)]
fn make_stale_socket(path: &Path) {
    use std::os::unix::net::UnixListener;
    // Bind to create the on-disk socket file, then drop the listener
    // so subsequent connect attempts get ECONNREFUSED — the canonical
    // shape for a socket left behind by a crashed nvim.
    let _l = UnixListener::bind(path).expect("bind unix socket");
}

/// Short-path tempdir for AF_UNIX socket fixtures. On macOS,
/// `std::env::temp_dir()` resolves to `/var/folders/xx/<long-hash>/T/`,
/// long enough that appending a fixture name pushes the full path past
/// sockaddr_un.sun_path's 104-byte cap and trips bind() with
/// `InvalidInput`. Pin under /tmp instead — short on every Unix.
#[cfg(unix)]
fn socket_safe_temp_dir() -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let pid = std::process::id();
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let d = PathBuf::from("/tmp").join(format!("todoke-cli-{stamp}-{pid}-{seq}"));
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[cfg(unix)]
#[test]
fn list_picks_up_filesystem_candidates_as_stale() {
    let dir = socket_safe_temp_dir();
    // Stage two real but unattended Unix sockets inside the tempdir;
    // the listen template points at the same dir via `vars.tmp`.
    make_stale_socket(&dir.join("nvim-todoke-default.sock"));
    make_stale_socket(&dir.join("nvim-todoke-git.sock"));
    // Decoys: regular file with .sock extension must be filtered out
    // by the is_socket() gate, and a non-matching name must miss the
    // skeleton entirely.
    std::fs::File::create(dir.join("nvim-todoke-imposter.sock")).unwrap();
    make_stale_socket(&dir.join("other.sock"));

    let config = dir.join("todoke.toml");
    write_file(
        &config,
        &format!(
            r#"
                [vars]
                tmp = "{tmp}"

                [todoke.nvim]
                kind = "neovim"
                command = "nvim"
                listen = "{{{{ vars.tmp }}}}/nvim-todoke-{{{{ group }}}}.sock"

                [[rules]]
                match = '.*'
                to = "nvim"
            "#,
            tmp = dir.display(),
        ),
    );

    let (ok, out, err) = run_with(&["--todoke-config", &config.to_string_lossy(), "list"]);
    assert!(ok, "stderr: {err}");
    assert!(out.contains("default"), "stdout: {out}");
    assert!(out.contains("git"), "stdout: {out}");
    // Both staged sockets are dead → reported as stale.
    assert!(out.contains("stale"), "stdout: {out}");
    // Decoys must not surface: regular file (imposter), or non-matching name (other).
    assert!(!out.contains("imposter"), "stdout: {out}");
    assert!(!out.contains("other.sock"), "stdout: {out}");

    // --alive-only filters them all out.
    let (ok2, out2, _) = run_with(&[
        "--todoke-config",
        &config.to_string_lossy(),
        "list",
        "--alive-only",
    ]);
    assert!(ok2);
    assert!(out2.contains("no instances found"), "stdout: {out2}");
}

#[cfg(unix)]
#[test]
fn kill_all_unlinks_stale_socket_files() {
    let dir = socket_safe_temp_dir();
    let stale_a = dir.join("nvim-todoke-default.sock");
    let stale_b = dir.join("nvim-todoke-git.sock");
    make_stale_socket(&stale_a);
    make_stale_socket(&stale_b);

    let config = dir.join("todoke.toml");
    write_file(
        &config,
        &format!(
            r#"
                [vars]
                tmp = "{tmp}"

                [todoke.nvim]
                kind = "neovim"
                command = "nvim"
                listen = "{{{{ vars.tmp }}}}/nvim-todoke-{{{{ group }}}}.sock"

                [[rules]]
                match = '.*'
                to = "nvim"
            "#,
            tmp = dir.display(),
        ),
    );

    let (ok, out, err) = run_with(&[
        "--todoke-config",
        &config.to_string_lossy(),
        "kill",
        "--all",
    ]);
    assert!(ok, "stderr: {err}");
    assert!(out.contains("stale, removed"), "stdout: {out}");
    assert!(!stale_a.exists(), "default socket should be unlinked");
    assert!(!stale_b.exists(), "git socket should be unlinked");
}
