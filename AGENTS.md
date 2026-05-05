# AGENTS.md

Guidance for AI agents (Claude / Codex / Gemini) working in this
repo. The yukimemi/* shared conventions live in the
`<!-- kata:agents:* -->` blocks below, sourced from
`yukimemi/pj-base` / `pj-rust` / `pj-rust-cli` via `kata apply` —
see those for git workflow, PR review cycle, build/lint/test
commands, release flow, and renri worktree usage.

The sections above the marker blocks are todoke-specific and
consumer-owned: edit them freely; `kata apply` won't touch them.

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
  space-joined argv), then per-argv (where **passthrough rules are
  tried before normal rules** for each argv, so `first_passthrough_match`
  wins over `first_match` when both would hit), then a passthrough
  attachment pass that merges passthrough hits into the existing
  `(target, group)` batch — see `dispatcher.rs:plan_batches`.
- **`joined` and `passthrough` are mutually exclusive.** Config
  rejects rules that set both. There's an open issue (#22) for an
  "extract" mode that would combine them, parked until a real config
  needs it.
- **`consumes` / `consumes_until` / `consumes_rest` are exclusive.**
  At most one may be set per passthrough rule (setting two or more is
  a compile error; setting none is fine — the rule then just
  passthroughs the single matched argv).
- **The TOML is pre-rendered in two passes.** The first pass is a
  manual line-scan in `extract_vars` to populate `[vars]` into the
  Tera context; the second pass is a single Tera render of the whole
  file with that context. Dispatch-time tokens (`{{ file_path }}`,
  `{{ group }}`, etc.) round-trip as self-referential strings so they
  survive pre-render intact.
- **`neovim` backend rejects non-file inputs.** URL / Raw inputs are
  warned and skipped — that's why non-GitHub URL rules need an
  `input_type = "url"` browser fallback.
- **Neovim remote mode can't deliver passthrough flags.** The RPC
  session is post-startup; a warn fires and the flags are dropped.
  Passthrough only reaches nvim on spawn paths (`new`, or remote
  fallback that spawns with `--listen`).

## Testing policy

Practice **TDD** (red-green-refactor):

1. Write (or extend) a failing test in `src/<mod>.rs::tests` for unit
   scope, or `tests/cli.rs` for end-to-end scope.
2. Implement until green.
3. Refactor with tests still green.

Commits should show tests arriving alongside the behaviour that
makes them pass — reviewers (and future you) read the test as the
authoritative spec, not "write code then bolt on tests
afterwards".

(Generic `cargo make` invocations are documented in the kata
`pj-rust` block below.)

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

<!-- kata:agents:base:begin -->
## yukimemi/* shared conventions

This file is the agent-agnostic source of truth (per the
[agents.md](https://agents.md) convention). The matching
`CLAUDE.md` and `GEMINI.md` files are thin shims that point back
here so each tool's auto-load behaviour still finds something.
**Edit AGENTS.md, not the shims.**

### Git workflow

- **No direct push to `main`.** Open a PR.
  - Exception: trivial typo / whitespace / docs wording fixes.
  - Exception: standalone version bumps.
- Branch names: `feat/...`, `fix/...`, `chore/...`.
- **PR titles + bodies in English. Commit messages in English.**
- Tag-based releases: `git tag vX.Y.Z && git push origin vX.Y.Z`.

### PR review cycle

- Every PR runs reviews from **Gemini Code Assist** and
  **CodeRabbit**. Wait for both bots to post, address their
  comments (push fixes to the PR branch), and merge only after
  feedback is resolved.
- **Reply to reviewers after pushing a fix.** Reply on the
  corresponding review thread with an **@-mention**
  (`@gemini-code-assist` / `@coderabbitai`). Silent fixes are
  invisible to reviewers and cost the audit trail.
- A review thread is **settled** the moment the latest bot reply
  is ack-only ("Thank you" / "Understood" / a re-review summary
  with no new findings) or 30 minutes elapse with no actionable
  comment.
- **Merge gate**: review bots quiet AND owner explicit approval.
- Bot-authored PRs (Renovate / Dependabot) skip the bot-review
  gate; CI green + owner approval is enough.

### Worktree workflow

Use [`renri`](https://github.com/yukimemi/renri) for any
commit-bound change. From the main checkout:

```sh
renri add <branch-name>            # create a worktree (jj-first)
renri --vcs git add <branch-name>  # force a git worktree
renri remove <branch-name>         # cleanup after merge
renri prune                        # GC stale worktrees
```

Read-only inspection can stay on the main checkout.

### kata-managed sections

Several files in this repo are managed by `kata apply` from the
[`yukimemi/pj-presets`](https://github.com/yukimemi/pj-presets)
templates — the bytes between `<!-- kata:*:begin -->` and
`<!-- kata:*:end -->` markers, plus the overwrite-always files
listed in `.kata/applied.toml`. **Editing those bytes locally
won't survive the next `kata apply`** — push the change to the
upstream template repo (`yukimemi/pj-base` / `yukimemi/pj-rust` /
…) instead. The marker scopes are layered:

- `kata:agents:base:*` — language-agnostic conventions (this section).
- `kata:agents:rust:*` — added when `pj-rust` applies.
- `kata:agents:rust-cli:*` — added when `pj-rust-cli` applies.
<!-- kata:agents:base:end -->
<!-- kata:agents:rust:begin -->
### Rust workflow

This repo follows the yukimemi/* Rust toolchain conventions. The
language-agnostic conventions block above (`kata:agents:base:*`)
covers git workflow, PR review cycle, and worktree usage.

### Build / lint / test

```sh
cargo make check                    # fmt --check + clippy + test + lock-check (the pre-push gate)
cargo make setup                    # one-time hook install + apm install
cargo build                         # debug build
cargo build --release               # release build
cargo test                          # tests; add -- --nocapture for stdout
```

`cargo make check` is what `.github/workflows/ci.yml` runs and what
the local pre-push hook calls — anything that passes locally
should pass on CI and vice versa. Don't paper over a failing
clippy by sprinkling `#[allow(clippy::...)]`; fix the underlying
issue or push back on the lint with reasoning.

### Toolchain pin

The Rust toolchain is pinned via `rust-toolchain.toml` and the
project compiles with the `stable` channel. Don't introduce
nightly-only features without a real reason; if you do, document
the reason in the relevant module.

### Lint / format policy

`rustfmt.toml` and `clippy.toml` are kata-managed (sourced from
`yukimemi/pj-rust`). Edits to those files in this repo won't
survive the next `kata apply`; if a setting is wrong, push the
fix to `yukimemi/pj-rust` so every yukimemi/* Rust project picks
it up.

### CI workflow

`.github/workflows/ci.yml` is also kata-managed. The source lives
in `yukimemi/pj-rust/.github/workflows/ci.yml.template` (the
`.template` suffix keeps GitHub Actions from running the source
itself in pj-rust); each Rust project receives the rendered
`ci.yml` via `kata apply`. Action versions are bumped centrally
by Renovate at `yukimemi/pj-rust` and propagate down on the next
apply, so don't bump them locally — Renovate is configured
(via the kata-distributed `renovate.json`) to ignore
`.github/workflows/ci.yml` and `.github/workflows/release.yml`
in each PJ to avoid the bump→clobber loop.
<!-- kata:agents:rust:end -->
<!-- kata:agents:rust-cli:begin -->
### Rust CLI release flow

This is a Rust CLI crate, so the release pipeline is publish-aware.
`yukimemi/pj-rust-cli` ships a tag-driven release workflow in
`.github/workflows/release.yml` (rendered from
`release.yml.template` for the same don't-auto-execute reason
ci.yml uses).

```sh
# Bump `package.version` in Cargo.toml (run `cargo build` so
# Cargo.lock follows), then:
git commit -am "chore: bump version to X.Y.Z"
git tag -a vX.Y.Z -m "vX.Y.Z"
git push origin main vX.Y.Z
```

The workflow then:
1. Cross-compiles binaries for x86_64 Linux / Windows / macOS,
   plus aarch64 macOS (Apple Silicon) — full triples
   `x86_64-unknown-linux-gnu`, `x86_64-pc-windows-msvc`,
   `x86_64-apple-darwin`, `aarch64-apple-darwin`.
2. Uploads them as a GitHub Release with auto-generated notes.
3. `cargo publish --locked` to crates.io using the
   `CARGO_REGISTRY_TOKEN` repo secret.

Set the `CARGO_REGISTRY_TOKEN` secret once per repo (`gh secret
set CARGO_REGISTRY_TOKEN`) before the first tag push. If the
crate is internal-only and shouldn't go to crates.io, either drop
the `publish` job locally (release.yml is `when = "once"` so the
edit survives subsequent applies) or set `package.publish = false`
in `Cargo.toml`.

The binary name is derived from the GitHub repo name at runtime
(`${{ github.event.repository.name }}`), so the workflow is
identical across yukimemi/* CLIs unless your `[[bin]] name` in
`Cargo.toml` deliberately differs from the repo name — in that
case override `BIN_NAME` in the workflow's `env:` block.
<!-- kata:agents:rust-cli:end -->
