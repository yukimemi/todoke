# <img src="assets/icon.png" width="32" align="left" alt="" /> todoke

<p align="center">
  <img src="assets/logo.svg" width="560" alt="todoke — rule-driven file dispatcher" />
</p>

<p align="center">
  <b>A rule-driven file dispatcher that hands incoming paths to the right editor or script — <i>届け</i>.</b>
</p>

<p align="center">
  <a href="https://crates.io/crates/todoke"><img src="https://img.shields.io/crates/v/todoke.svg" alt="crates.io"/></a>
  <a href="https://github.com/yukimemi/todoke/actions"><img src="https://github.com/yukimemi/todoke/actions/workflows/ci.yml/badge.svg" alt="CI"/></a>
  <a href="./LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="License: MIT"/></a>
</p>

```
┌──────┐       ┌────────┐       ╭──▶ nvim
│ file │ ──▶   │ todoke │ ──▶   ├──▶ code
└──────┘       └────────┘       ╰──▶ script / …
```

`todoke` takes one or more file paths and decides what to do with each of
them — by regex-matching the path against a TOML ruleset. A rule can target
a long-running neovim (reused via msgpack-RPC), any generic CLI editor, or a
raw shell script. Perfect as your OS default program for text files, as
`$EDITOR`, or as a standalone file handler.

It is the successor to [`edtr`][edtr] / [`hitori.vim`][hitori], generalized
from "editor router" into a full rule-driven dispatcher.

## Features

- **Rule-based routing**: regex patterns in TOML decide what handles each
  file. Different paths → different handlers (VSCode for one project, nvim
  for another, a shell script for a third).
- **Single-instance neovim** via named pipes / unix sockets: `todoke`
  connects to a running nvim and sends `:edit` over msgpack-RPC. Works on
  Windows via `\\.\pipe\...` — no Deno, no plugin framework, no cold start.
- **Sync or async** per rule: `sync = true` blocks until the handler exits
  (perfect for `git commit`), `sync = false` fires and forgets (perfect for
  double-clicking files in the OS file explorer).
- **Tera templating** throughout the config: `{{ file_path }}`,
  `{{ env.HOME }}`, `{% if is_windows() %}…{% endif %}`, structural
  conditionals that include whole editor / rule blocks, every Tera filter.
- **Generic CLI support**: any command-line tool works (`code`, `vim`,
  `helix`, `subl`, `emacsclient`, `bat`, `pandoc`, …) without custom code.
- **`edtr` compatibility**: same embedded default config, same config
  schema. Existing `edtr` users migrate by renaming the config directory
  (see below).
- **Fast**: static Rust binary, cold start in milliseconds. On Windows this
  is often 10–100× faster than denops-based alternatives.

## Install

```sh
cargo install todoke
```

Binary lives at `~/.cargo/bin/todoke`. Make sure that's on your `PATH`.

## Quick start

`todoke` works out of the box with a bundled default config — it routes
everything to a single shared neovim instance, except `$EDITOR`-callback
files (`COMMIT_EDITMSG` etc.) which always get a fresh `sync = true`
instance so `git commit` works.

To customize, drop a file at:

- Linux / macOS / Windows: `~/.config/todoke/todoke.toml`

Minimal example:

```toml
# ~/.config/todoke/todoke.toml

# kind = "neovim" opts into msgpack-RPC reuse; "exec" (default) just spawns.
[todoke.nvim]
kind = "neovim"
command = "nvim"
listen = '{% if is_windows() %}\\.\pipe\nvim-todoke-{{ group }}{% else %}/tmp/nvim-todoke-{{ group }}.sock{% endif %}'

[todoke.code]
command = "code"
[todoke.code.args]
remote = ["--reuse-window"]
new    = ["--new-window"]

[todoke.firefox]
command = "firefox"

# A second firefox target specifically for issue: inputs — the URL is
# constructed from the capture group, so append_inputs = false tells the
# exec backend not to tack the raw "issue:42" onto the command line as a
# second positional.
[todoke.gh-issue]
command = "firefox"
append_inputs = false
args.default = ["https://github.com/yukimemi/todoke/issues/{{ cap.1 }}"]

# Git-ref target: opens the GitHub tree browser at a branch / tag / sha.
[todoke.gh-ref]
command = "firefox"
append_inputs = false
args.default = ["https://github.com/yukimemi/todoke/tree/{{ input }}"]

# git commit, rebase, etc. — always a blocking fresh nvim.
[[rules]]
name = "editor-callback"
match = '(?i)/(COMMIT_EDITMSG|MERGE_MSG|git-rebase-todo)$'
to = "nvim"
mode = "new"
sync = true

# GitHub URLs → firefox (URL is auto-appended by the exec backend)
[[rules]]
name = "gh"
match = '^https?://(www\.)?github\.com/'
to = "firefox"

# Route files under ~/src/company/ to VSCode.
[[rules]]
name = "work"
match = '/src/company/'
to = "code"
mode = "remote"

# Raw strings — custom-scheme bare ids like `issue:42` auto-detect as Raw
# so this rule fires without `--as`. Capture groups are available to the
# handler as `{{ cap.1 }}` / `{{ cap.name }}`.
[[rules]]
name = "gh-issue"
match = '^issue:(\d+)$'
to = "gh-issue"

# Git refs — branch names, tags, short SHAs, etc. `input_type = "raw"`
# pins this rule to `--as raw` so that bare words like `HEAD` / `main`,
# which auto-detect as File, don't accidentally trigger the GitHub URL
# handler when you meant to open a local file by that name.
[[rules]]
name = "gh-ref"
match = '^(HEAD|main|master|develop|v?\d+\.\d+\.\d+|[0-9a-f]{7,40})$'
to = "gh-ref"
input_type = "raw"

# URL fallback: any other URL → browser. Without this, non-GitHub URLs
# would fall through to the file default (nvim) and get dropped by the
# neovim backend (which only accepts files).
[[rules]]
name = "url-default"
match = '^https?://'
input_type = "url"
to = "firefox"

# Default: everything else (file inputs, mostly) goes to the shared nvim.
[[rules]]
name = "default"
match = '.*'
to = "nvim"
group = "default"
mode = "remote"
```

Then:

```sh
# Open any file in the right handler
todoke notes.md

# URLs work too — same rule engine routes them to a browser, a browser
# profile, or any CLI that accepts URLs.
todoke https://github.com/yukimemi/todoke  # → gh rule → firefox
todoke https://example.com                  # → url-default rule → firefox

# Raw strings match rules too. `<scheme>:<body>` bare ids auto-detect as
# Raw so gh-issue fires without `--as`. Captures are available as
# `{{ cap.N }}`.
todoke issue:42      # → firefox opens issues/42

# Bare words like `HEAD` or `Makefile` auto-detect as File (so
# `$EDITOR=todoke Makefile` Just Works — see the $EDITOR section below).
# When you want `HEAD` routed as a git ref instead, pass `--as raw` and
# wire the matching rule with `input_type = "raw"`:
todoke --as raw HEAD # → firefox opens the repo tree at HEAD

# See which rule would match, without actually dispatching
todoke check notes.md https://example.com issue:42

# Same dispatch logic, don't execute
todoke --dry-run notes.md

# Lint the config for common footguns
todoke doctor
```

### Recipe: one target, many variants

Neovim has several front-ends — `nvim` itself, `neovide`, `nvim-qt`, … —
and you'll probably want to swap between them without rewriting rules.
Because the whole config is pre-rendered through Tera, a list in `[vars]`
plus a single conditional covers every combination:

```toml
[vars]
# Swap this line to switch front-ends.
gui = "neovide"
# Wrappers that forward CLI args to an embedded nvim only after `--`.
# Raw `nvim` is not in this list because it would treat args after `--`
# as filenames.
wrapper_guis = ["neovide", "nvim-qt"]

[todoke.gui]
kind = "neovim"
command = "{{ vars.gui }}"
listen = '{% if is_windows() %}\\.\pipe\nvim-todoke-{{ group }}{% else %}/tmp/nvim-todoke-{{ group }}.sock{% endif %}'

{% if vars.gui in vars.wrapper_guis %}
[todoke.gui.args]
remote = ["--"]
{% endif %}

[[rules]]
match = '.*'
to = "gui"
mode = "remote"
```

- `vars.gui = "nvim"` → `nvim FILE --listen PIPE`
- `vars.gui = "neovide"` → `neovide FILE -- --listen PIPE`
- `vars.gui = "nvim-qt"` → `nvim-qt FILE -- --listen PIPE`

One target definition, three valid command lines. Adding a new wrapper in
the future is one entry in `wrapper_guis`.

### Recipe: categorized `match` patterns

`match` accepts either a single regex string or an array. The array form
is OR-matched (hit any → rule fires) and is the right shape when a rule's
intent spans several unrelated sources — `$EDITOR`-callback files are a
classic example because every tool sprinkles its own filename convention:

```toml
[[rules]]
name = "editor-callback"
match = [
  # git
  '(?i)/(COMMIT_EDITMSG|MERGE_MSG|TAG_EDITMSG|EDIT_DESCRIPTION|git-rebase-todo|NOTES_EDITMSG|\.gitmessage)$',
  # svn / hg
  '(?i)/svn-commit\.tmp$',
  # Claude Code prompt temp files
  '(?i)/claude-prompt-.*$',
]
to = "nvim-term"
mode = "new"
sync = true
```

Each bucket is its own readable regex; extending for a new tool is
appending one line with a `# new-tool` comment instead of threading
another alternation into a long single-string pattern.

### Recipe: editor-flag passthrough (`+42 file.txt`)

Some `$EDITOR` callers (vim-aware Git frontends, `sudo -e`, etc.) pass
`nvim`-style flags ahead of the file — e.g. `+42 file.txt` to jump to
line 42. todoke's auto-detection would otherwise absolutize `+42` into
a file path. Two ways to handle it:

**Option A — `passthrough`** (simple; good for individual flag classes):

```toml
[[rules]]
name = "nvim-flag"
match = '^[-+]'          # matches against the RAW argv, pre auto-detect
to = "nvim-term"
sync = true
passthrough = true       # forward as-is to nvim's start-up argv

[[rules]]
name = "nvim-file"
match = '.*'
to = "nvim-term"
sync = true
```

`todoke +42 foo.txt bar.txt` now spawns `nvim +42 foo.txt bar.txt`
(multi-file still works, `+42` rides along as a flag). A separate
`-c :set ...` rule can be added the same way.

Passthrough inputs are merged into the **normal rule's batch** that
shares the same `(target, group)` — so a passthrough rule's `mode` /
`sync` are only used when no normal rule routes to the same
target+group. On a merge the normal rule's values win and a runtime
warn is emitted if they differ (doctor can't catch it because
`group` / `to` are Tera templates that only resolve at dispatch).

**Option B — `joined`** (flexible; one rule captures the whole argv):

```toml
[[rules]]
name = "nvim-with-line"
match = '^(?P<pre>\+\d+ )?(?P<input>\S+)$'
to = "nvim-term"
sync = true
joined = true

[todoke.nvim-term.args]
default = ["{{ cap.pre | default(value='') | trim }}"]
# append_inputs = true is still default so {{ cap.input }} is opened
# by the handler after args; the captured flag rides in the args list.
```

`joined` matches once against the space-joined argv. The named capture
`input` is re-classified (so a nonexistent `foo.txt` still becomes a
`File` and `:edit`-able), and `cap.pre` ends up in the args. Use
`joined` when you want a single regex describing the full invocation
shape; use `passthrough` when each flag has its own rule.

### As `$EDITOR`

```sh
export EDITOR=todoke
git commit      # → todoke routes COMMIT_EDITMSG to nvim mode=new sync=true
```

The bundled default config is compatible with every `$EDITOR=…` caller I
know of (git, crontab, visudo, fc, mutt, …).

Any arg that isn't a URL (`foo://…`) or a custom-scheme bare id
(`issue:42`) auto-detects as a **file** — including extension-less
names like `Makefile`, `Dockerfile`, `Rakefile` and not-yet-existing
paths like `newfile.txt` or `/tmp/new.md`. So `todoke Makefile` and
`todoke newfile.txt` behave just like `vim Makefile` / `vim newfile.txt`
— rules match against the absolute path and the editor creates the
file on write.

### As OS default program (Windows)

Right-click a `.txt` → Open with → Choose another app → Browse → point at
`todoke.exe`. `todoke` honors the rules and opens the file in the correct
handler, spawning a new console if the target is a TUI.

## Configuration reference

### `[vars]`

User-defined variables available as `{{ vars.NAME }}` in every other
template:

```toml
[vars]
proj_root = "/home/me/src"
```

### `[todoke.<name>]`

A delivery target (the value behind a rule's `to = "<name>"`).

| field      | type                                | required | meaning                                                         |
| ---------- | ----------------------------------- | -------- | --------------------------------------------------------------- |
| `kind`     | `"exec"` / `"neovim"`               | no (default `"exec"`) | `"exec"` spawns the command; `"neovim"` reuses a running nvim via msgpack-RPC |
| `command`  | string                              | yes      | the handler binary (PATH-resolved)                              |
| `listen`   | string                              | neovim   | socket / named pipe path for RPC                                |
| `args`     | table of `<mode>` → `array<string>` | no       | args injected based on `rule.mode`; `args.default` is the fallback when no key matches |
| `append_inputs` | bool                           | `true`   | `exec` kind only: whether each input's display string is appended as a trailing positional arg after `args`. Set to `false` when `args` already reference the input via `{{ input }}` / `{{ cap.N }}` and you don't want the raw value passed twice. |
| `env`      | table                               | no       | env vars passed to the spawned handler                          |

### `[[rules]]`

| field     | type                      | default      | meaning                                      |
| --------- | ------------------------- | ------------ | -------------------------------------------- |
| `name`    | string                    | `rule[N]`    | human-readable label (shown in `check`)      |
| `match`   | regex string or `[regex]` | required     | pattern(s) matched against a string derived from the input: **file** = canonicalized absolute path with `/` separators (`\\?\` verbatim prefix stripped), **url** = the URL string as-is, **raw** = the argument string as-is. Anchors like `^foo$` only fire for the URL/raw cases unless you design the regex for absolute paths. |
| `exclude` | regex string or `[regex]` | none         | when any `exclude` hits, the rule is skipped even if `match` hits — todoke falls through to the next rule |
| `to`      | string (Tera-templated)   | required     | key into `[todoke.*]`                        |
| `group`   | string                    | `"default"`  | instance identity (one nvim per group)       |
| `mode`    | string                    | `"remote"`   | free-form; `"remote"` / `"new"` are reserved for neovim behavior, otherwise used only to pick `args.<mode>` |
| `sync`    | bool                      | `false`      | `true` = block until handler exits           |
| `input_type` | `"file" \| "url" \| "raw"` or array | all kinds | restrict which input kinds this rule applies to. Example: `input_type = "raw"` makes the rule fire only for `--as raw` / auto-detected Raw inputs — useful for git-ref style patterns (`^HEAD$`, `^main$`) that must not shadow a local file of the same name. |
| `joined`   | bool                       | `false`     | match against the full argv-join (all positional args concatenated with spaces, **pre auto-detect**) instead of each input individually. On a hit, the named capture `input` is re-classified via `Input::from_arg` and becomes the batch's sole input; other captures ride along in `{{ cap.<name> }}` for the target's args templates. Designed for `$EDITOR=todoke +42 file.txt` style calls. Mutually exclusive with `passthrough`. |
| `passthrough` | bool                    | `false`     | match against the **raw argv** (pre auto-detect) per input. On a hit, the raw string is forwarded to the target's start-up argv instead of being opened/edited. Use for editor flags like `+42` / `-c :set ft=...`. Mutually exclusive with `joined`. |

### Template context

Available in `rule.group`, `rule.to`, `todoke.*.command`, `todoke.*.listen`,
`todoke.*.args.*`:

| variable        | example                             | populated for |
| --------------- | ----------------------------------- | ------------- |
| `input`         | `/tmp/foo.md` or `https://…`        | always        |
| `input_type`    | `"file"` / `"url"` / `"raw"`         | always        |
| `file_path`     | `C:/Users/you/notes/todo.md`        | file inputs   |
| `file_dir`      | `C:/Users/you/notes`                | file inputs   |
| `file_name`     | `todo.md`                           | file inputs   |
| `file_stem`     | `todo`                              | file inputs   |
| `file_ext`      | `md` (no leading dot)               | file inputs   |
| `url_scheme`    | `https`                             | URL inputs    |
| `url_host`      | `github.com`                        | URL inputs    |
| `url_port`      | `443` or empty                      | URL inputs    |
| `url_path`      | `/yukimemi/todoke`                  | URL inputs    |
| `url_query`     | `tab=rs` or empty                   | URL inputs    |
| `url_fragment`  | `top` or empty                      | URL inputs    |
| `command_*`     | same five fields for the target command | always    |
| `cwd`           | current working directory           | always        |
| `group`         | resolved group                      | phase 3       |
| `rule`          | resolved rule name                  | phase 3       |
| `cap.0`         | full match of the `match` regex     | when a rule matched |
| `cap.1` / `cap.2` / … | numbered capture groups       | when defined        |
| `cap.<name>`    | named capture groups `(?P<name>…)`  | when defined        |
| `vars.<key>`    | your `[vars]` entries               | always        |
| `env.<KEY>`     | process env at todoke invocation    | always        |

`kind = "neovim"` targets accept **file inputs only** — URLs and raw
strings routed to a neovim target are logged and skipped. Route those to
a `kind = "exec"` target (e.g. a browser for URLs, any CLI that consumes
the raw string for `"raw"`).

And these todoke-specific Tera functions:

- `is_windows()`, `is_linux()`, `is_mac()` — booleans for OS branching.

Plus everything Tera ships — `replace`, `split`, `join`, `length`, `now()`,
structural `{% if %}` / `{% elif %}` / `{% else %}` blocks around editor
and rule sections, and all other stock [Tera features][tera].

## CLI reference

```
todoke [FILES]...            # dispatch files per rules (default action)
todoke check <FILES>...      # dry-run: show matched rule per file
todoke doctor                # lint the config for common footguns
todoke completion <shell>    # emit shell completion script
todoke --help
todoke --version

# v0.2+:
todoke list                    # list alive handler instances
todoke kill <group> | --all    # terminate instances
todoke config path | edit | validate | show
```

Flags:

- `-c, --config <PATH>` — override config path
- `-E, --editor <NAME>` — bypass rule, force handler
- `-G, --group <NAME>`  — bypass rule, force group
- `--dry-run`           — print the resolved plan without executing
- `-v, --verbose`       — `-v` = info, `-vv` = debug, `-vvv` = trace

Logging is also controllable via `RUST_LOG`.

## Roadmap

- **v0.1** *(this release)*: core dispatch, neovim + generic backends,
  `check`, `doctor`, `completion`, default config, `$EDITOR`
  compatibility, colored output.
- **v0.2**: `list` / `kill` / `config edit|validate|show`, `open` / `send`,
  neovim `remote + sync` via `nvim_buf_attach`.
- **v0.3**: `script` editor kind — run arbitrary shell commands as a
  handler, turning todoke into a general "open with rules" tool for any
  file type (previewer, formatter, pipeline, …).

## Heritage

`todoke` extends [`edtr`][edtr], which was itself a Rust rewrite of
[`hitori.vim`][hitori]. The lineage:

- `hitori.vim` (denops): single-instance vim plugin, vim/neovim-only, slow
  on Windows.
- `edtr`: Rust rewrite, editor-agnostic, fast on all platforms.
- `todoke`: `edtr` plus broader scope — any command-line handler (not just
  editors), any file type. The name 「届け」 means *deliver* in Japanese.

## License

[MIT](./LICENSE) — © 2026 yukimemi.

[tera]: https://keats.github.io/tera/docs/#built-ins
[hitori]: https://github.com/yukimemi/hitori.vim
[edtr]: https://crates.io/crates/edtr
