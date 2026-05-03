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
cargo make setup              # one-time on clone: pre-push hook + APM install
cargo test                    # unit + integration
cargo test --test cli         # integration only
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```

`cargo make check` runs the whole pre-push suite (fmt / clippy / test /
locked build). A pre-push hook invokes it automatically.

`cargo make setup` is `hook-install` + `apm-install`. The latter
requires the [APM](https://github.com/microsoft/apm) CLI on `PATH`
(`scoop install apm` on Windows, `brew install microsoft/apm/apm`
on macOS, `pip install apm-cli`, or `curl -sSL https://aka.ms/apm-unix | sh`).
It runs `apm install`, which compiles the
[renri](https://github.com/yukimemi/renri) skill (declared in
`apm.yml`, pinned to `#main`) into `.claude/skills/` +
`.gemini/skills/` + `.github/skills/` so AI sessions know how to
manage worktrees / jj workspaces while developing todoke. Lockfile is `apm.lock.yaml`.
Pinned to `#main`, so `apm install --update` always pulls the latest renri skill content.

## Working in this repo with AI agents

- **Read-only inspection** (browsing files, answering questions,
  running read-only commands): no worktree needed; work in the
  existing checkout.
- **Any commit-bound change** — new feature, bug fix, refactor,
  reviewer-feedback fix on an open PR: if you are on the **main
  checkout**, start with `renri add <branch-name>` and move into
  the worktree before committing (`cd "$(renri cd <branch-name>)"`,
  or use the shell wrapper from `renri shell-init` so plain
  `renri cd <name>` cds for you). If you are **already in a
  worktree** (e.g. iterating on an existing PR), keep working
  there. Do **not** edit on the main checkout for non-trivial
  changes.
- **Trivial wording / typo fixes** are the only soft exception, and
  even then `renri add` is cheap enough that defaulting to it is
  fine.

### Backend choice — jj-first

This repo is colocated git+jj. `renri add` defaults to **jj**
(creates a non-colocated jj workspace where `jj` commands work and
`git` does not — see [jj-vcs/jj#8052](https://github.com/jj-vcs/jj/issues/8052)
for why secondary colocation isn't possible yet). Stick to the
default unless there is a specific reason to use git tooling.

```sh
# In a freshly created worktree (default jj backend):
jj st                                               # status
jj describe -m "feat: ..."                          # set @-commit description
jj git push --bookmark <branch-name> --allow-new    # first push of a new branch
jj git push --bookmark <branch-name>                # subsequent pushes
```

`renri --vcs git add <branch-name>` is the override and exists for
genuine git-CLI-only needs (git submodule, native git2 tooling,
git-only hooks). Do **not** reach for it out of git-CLI familiarity
— prefer learning the equivalent jj commands.

### Cleanup after merge

After the PR merges and you've pulled the change into main:

- `renri remove <branch>` — removes a single worktree. Calls
  `git worktree remove` or `jj workspace forget` as appropriate,
  then deletes the directory. Refuses to remove the main worktree.
- `renri prune` — best-effort GC across the repo. Git: removes
  worktree metadata for already-deleted directories. jj: forgets
  workspaces whose root path is gone (the missing
  `jj workspace prune` analog).

Run `renri prune` periodically — especially after manually
`rm -rf`-ing worktree dirs without going through `renri remove`.

### Hooks in worktrees

The pre-push hook installed by `cargo make hook-install` lives in
the **main repo's** `.git/hooks/pre-push`.

- **git worktrees** share that hook directory, so plain `git push`
  from a worktree triggers `cargo make check` automatically.
- **jj workspaces** route their pushes through `jj git push`, which
  uses libgit2 directly and **does not fire git hooks**. From a jj
  workspace, run `cargo make check` manually before
  `jj git push --bookmark <branch-name>` — there is no automatic gate.

### Post-create automation (`cargo make on-add`)

`renri.toml` declares a `[[hooks.post_create]]` that runs
`cargo make on-add` immediately after `renri add` finishes. The
default chain is:

- `apm install --update` — refresh the renri skill so AI agents in
  the new worktree see the latest guidance.
- `vcs-fetch` — `jj git fetch` in a jj workspace, `git fetch`
  otherwise; cleans up subsequent rebase / merge.

Add per-repo extras (e.g. `cargo fetch`) by extending
`[tasks.on-add]`'s dependency list in `Makefile.toml`.

## Contribution workflow

- **No direct pushes to `main`.** Open a PR instead.
  - Exception: trivial typo fixes, whitespace-only commits,
    documentation wording tweaks. When in doubt, PR it.
  - Exception: **release version bumps.** A standalone
    `Cargo.toml` version bump (plus the `cargo check` lockfile
    refresh and the `git tag vX.Y.Z && git push origin vX.Y.Z`
    that follows) is also fine on `main` directly — a PR for a
    one-line bump is more noise than signal.
- Every PR triggers **Gemini Code Assist** and **CodeRabbit** reviews.
  Wait for both to post, address their comments (push fixes to the PR
  branch), and only merge once the feedback is resolved.
- **Reply to the reviewer after pushing a fix.** For every review
  comment you act on, post a reply in that comment's thread with an
  **@-mention of the reviewer** (`@gemini-code-assist`,
  `@coderabbitai`) so they know the feedback has been addressed.
  A silent fix is invisible to the reviewer — they'll re-review
  blindly, and you lose the audit trail that ties the fix commit to
  the original concern.
- **Merge gating.** Do NOT merge until **both** conditions are true:
  1. Every review bot (Gemini, CodeRabbit) has stopped posting new
     actionable comments — keep iterating fix → @-mention → wait
     until they go quiet.
  2. The repo owner (@yukimemi) has given explicit approval to merge.
  Acknowledgement-only replies from a bot ("Understood", "Thank you")
  count as a quiet pass for that thread. New actionable findings
  restart the loop.
- **Exception: bot-authored PRs (Renovate, Dependabot).** Gemini and
  CodeRabbit skip them by default, so the "wait for bot review" gate
  doesn't apply. Merge as long as CI is green and the owner approves.
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
