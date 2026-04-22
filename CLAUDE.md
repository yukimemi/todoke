# CLAUDE.md

Guidance for Claude Code when working in this repo.

## What todoke is

A rule-driven file/URL/raw-string dispatcher, written in Rust. Takes
positional args (`todoke <arg> <arg> ...`), classifies each as a
`File` / `Url` / `Raw`, runs it against TOML-defined `[[rules]]`, and
hands the match to the rule's `to = "<target>"` — either an `exec`
backend (spawn a command) or a `neovim` backend (msgpack-RPC reuse of
a running nvim).

Name comes from 届け (todoke, "deliver"), successor to `edtr` / `hitori.vim`.

## Source layout

- `src/main.rs` — entry point, wires `clap` to `dispatcher::dispatch`.
- `src/cli.rs` — clap `Cli` struct.
- `src/input.rs` — `Input` enum (`File`/`Url`/`Raw`), auto-detect
  (URL → existing file → custom-scheme → invalid-chars → File).
- `src/config.rs` — TOML schema (`Config`, `Target`, `Rule`,
  `InputTypes`), Tera pre-render, `ResolvedConfig` with compiled regexes.
- `src/matcher.rs` — `first_match` / `first_joined_match` /
  `first_passthrough_match`; path normalization.
- `src/dispatcher.rs` — the orchestrator. `plan_batches` builds the
  batches (joined phase → per-argv pass 2a → passthrough pass 2b);
  `run_batch` dispatches a batch to its backend.
- `src/backends/exec.rs`, `src/backends/neovim.rs` — the two backends.
- `src/template.rs` — Tera engine + context builder.
- `src/platform.rs` — `spawn_detached` shim (cmd /c start on Windows).
- `src/style.rs` — TTY color helpers.
- `tests/cli.rs` — integration tests, `cargo run` via
  `env!("CARGO_BIN_EXE_todoke")`.

## Key design decisions (don't rediscover)

- **Auto-detect is file-first.** Anything that isn't a URL
  (`scheme://`) or a custom-scheme id (`issue:42`) or a string with
  path-invalid chars (`<>"|?*`, controls) classifies as `File`. Bare
  `Makefile` / `HEAD` / `newfile.txt` all become `File` so editors can
  open / create them. Rules that want `Raw` semantics must opt in with
  `input_type = "raw"`.
- **Rule matching runs in three phases.** Joined first (regex against
  space-joined argv), then per-argv normal (input's `match_string`),
  then passthrough (second pass that attaches to existing
  `(target, group)` batches — see `dispatcher.rs:plan_batches`).
- **`joined` and `passthrough` are mutually exclusive.** Config
  rejects rules that set both. There's an open issue (#22) for an
  "extract" mode that would combine them, parked until a real config
  needs it.
- **`consumes` / `consumes_until` / `consumes_rest` are exclusive.**
  Exactly one may be set per passthrough rule.
- **Tera pre-renders the TOML twice.** First pass extracts `[vars]` by
  line-scan; second pass renders the whole file. Dispatch-time tokens
  (`{{ file_path }}`, `{{ group }}`, etc.) round-trip as
  self-referential strings so they survive pre-render intact.
- **`neovim` backend rejects non-file inputs.** URL / Raw inputs are
  warned and skipped — that's why non-GitHub URL rules need an
  `input_type = "url"` browser fallback.
- **Neovim remote mode can't deliver passthrough flags.** The RPC
  session is post-startup; a warn fires and the flags are dropped.
  Passthrough only reaches nvim on spawn paths (`new`, or remote
  fallback that spawns with `--listen`).

## Testing

Practice **TDD**. For any new behavior:

1. Write (or extend) a failing test in `src/<mod>.rs::tests` for unit
   scope, or `tests/cli.rs` for end-to-end scope.
2. Implement until green.
3. Refactor with tests still green.

Red-green-refactor, not "write code then bolt on tests afterwards".
Commits should show tests arriving alongside the behavior that makes
them pass — reviewers (and future you) read the test as the
authoritative spec.

Run:

```sh
cargo test                    # unit + integration
cargo test --test cli         # integration only
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```

`cargo make check` runs the whole pre-push suite (fmt / clippy / test /
locked build). A pre-push hook invokes it automatically.

## Contribution workflow

- **No direct pushes to `main`.** Open a PR instead.
  - Exception: trivial typo fixes, whitespace-only commits,
    documentation wording tweaks. When in doubt, PR it.
- Every PR triggers **Gemini Code Assist** and **CodeRabbit** reviews.
  Wait for both to post, address their comments (push fixes to the PR
  branch), and only merge once the feedback is resolved.
- Tag-based releases: `git tag vX.Y.Z && git push origin vX.Y.Z`
  triggers the GitHub Actions release workflow.

## Useful invocations

```sh
# Dry-run a dispatch (shows planned batches, doesn't spawn)
cargo run --quiet -- --dry-run -- +42 foo.txt

# Override config path for experiments
TODOKE_CONFIG=/tmp/exp.toml cargo run --quiet -- --dry-run -- <args>

# Config static analysis
cargo run --quiet -- doctor

# Show which rule matches each arg without dispatching
cargo run --quiet -- check foo.txt https://example.com issue:42
```

## Version + changelog

Version lives only in `Cargo.toml`. `cargo check` refreshes `Cargo.lock`
after a bump. Commit titles follow `<type>: <summary> (vX.Y.Z)` (e.g.
`feat: ... (v0.3.19)`) so the release surface is traceable from `git log`.
