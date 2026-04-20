# edtr

<p align="center">
  <img src="assets/logo.svg" width="520" alt="edtr — editor router" />
</p>

<p align="center">
  <b>An editor router/transfer tool that dispatches files to the right editor based on rules.</b>
</p>

<p align="center">
  <a href="https://crates.io/crates/edtr"><img src="https://img.shields.io/crates/v/edtr.svg" alt="crates.io"/></a>
  <a href="https://github.com/yukimemi/edtr/actions"><img src="https://github.com/yukimemi/edtr/actions/workflows/ci.yml/badge.svg" alt="CI"/></a>
  <a href="./LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="License: MIT"/></a>
</p>

```
┌──────┐       ┌──────┐       ╭──▶ nvim
│ file │ ──▶   │ edtr │ ──▶   ├──▶ code
└──────┘       └──────┘       ╰──▶ …
```

`edtr` is a fast, cross-platform CLI that takes one or more file paths and
forwards them to the right editor according to rules you write in TOML. It is
the Rust successor to [`hitori.vim`](https://github.com/yukimemi/hitori.vim):
same "single editor instance" idea, but editor-agnostic and with near-zero
startup cost — perfect for registering as your OS default program for text
files, or as `$EDITOR`.

## Features

- **Rule-based routing**: regex patterns in TOML decide which editor opens
  which file. Different paths can route to different editors (VSCode for one
  project, nvim for another).
- **Single-instance neovim** via named pipes / unix sockets: `edtr` connects
  to a running nvim and sends `:edit` over msgpack-RPC. Works on Windows via
  `\\.\pipe\...` — no Deno, no plugin framework, no cold start.
- **Sync or async** per rule: `sync = true` waits for the editor to exit
  (perfect for `git commit`), `sync = false` fires and forgets (perfect for
  double-clicking files in the OS file explorer).
- **Tera templating** throughout the config: `{{ file_path }}`,
  `{{ env.HOME }}`, `{% if is_windows() %}...{% endif %}`, and every Tera
  filter.
- **Generic editor support**: any CLI editor works (`code`, `vim`, `helix`,
  `subl`, `emacsclient`, …) without custom code.
- **Fast**: static Rust binary, cold start in milliseconds. On Windows this
  is often 10–100× faster than denops-based alternatives.

## Install

```sh
cargo install edtr
```

Binary lives at `~/.cargo/bin/edtr`. Make sure that's on your `PATH`.

## Quick start

`edtr` works out of the box with a bundled default config — it routes
everything to a single shared neovim instance, except `$EDITOR`-callback
files (`COMMIT_EDITMSG` etc.) which always get a fresh `sync = true` instance
so `git commit` works.

To customize, drop a file at:

- Linux / macOS: `~/.config/edtr/edtr.toml`
- Windows: `%APPDATA%\edtr\edtr.toml`

Minimal example:

```toml
# ~/.config/edtr/edtr.toml

[editors.nvim]
kind = "neovim"
command = "nvim"
# The pipe used to reach the running nvim. is_windows()/is_linux()/is_mac()
# are edtr-provided Tera functions.
listen = '{% if is_windows() %}\\.\pipe\nvim-edtr-{{ group }}{% else %}/tmp/nvim-edtr-{{ group }}.sock{% endif %}'

[editors.code]
kind = "generic"
command = "code"
args_remote = ["--reuse-window"]
args_new = ["--new-window"]

# git commit, rebase, etc. — always a blocking fresh nvim.
[[rules]]
name = "editor-callback"
match = '(?i)/(COMMIT_EDITMSG|MERGE_MSG|git-rebase-todo)$'
editor = "nvim"
mode = "new"
sync = true

# Route files under ~/src/company/ to VSCode.
[[rules]]
name = "work"
match = '/src/company/'
editor = "code"
mode = "remote"

# Default: everything else goes to the shared nvim.
[[rules]]
name = "default"
match = '.*'
editor = "nvim"
group = "default"
mode = "remote"
```

Then:

```sh
# Open any file in the right editor
edtr notes.md

# See which rule would match, without actually dispatching
edtr check notes.md src/main.rs

# Same dispatch logic, just don't execute
edtr --dry-run notes.md
```

### As `$EDITOR`

```sh
export EDITOR=edtr
git commit      # → edtr routes COMMIT_EDITMSG to nvim mode=new sync=true
```

The bundled default config is compatible with every `$EDITOR=...` caller I
know of (git, crontab, visudo, fc, mutt, …).

### As OS default program (Windows)

Right-click a `.txt` → Open with → Choose another app → Browse → point at
`edtr.exe`. `edtr` will honor the rules and open the file in the correct
editor, spawning a new console if the target editor is a TUI.

## Configuration reference

### `[vars]`

User-defined variables available as `{{ vars.NAME }}` in every other
template:

```toml
[vars]
proj_root = "/home/me/src"
```

### `[editors.<name>]`

| field         | type           | required | meaning                                                |
| ------------- | -------------- | -------- | ------------------------------------------------------ |
| `kind`        | `"neovim"` / `"generic"` | yes      | backend selection                                      |
| `command`     | string         | yes      | the editor binary (PATH-resolved)                      |
| `listen`      | string         | neovim   | socket / named pipe path for RPC                       |
| `args_new`    | array\<string> | no       | extra args when `mode = "new"`                         |
| `args_remote` | array\<string> | no       | extra args when spawning for `mode = "remote"` fallback |
| `env`         | table          | no       | env vars passed to the spawned editor                  |

### `[[rules]]`

| field    | type                     | default      | meaning                                      |
| -------- | ------------------------ | ------------ | -------------------------------------------- |
| `name`   | string                   | `rule[N]`    | human-readable label (shown in `check`)      |
| `match`  | regex string or `[regex]` | required     | path pattern(s); paths are normalized to `/` before matching |
| `editor` | string                   | required     | key from `[editors.*]`                       |
| `group`  | string                   | `"default"`  | instance identity (one nvim per group)       |
| `mode`   | `"remote"` / `"new"`     | `"remote"`   | `remote` = reuse existing, `new` = always fresh |
| `sync`   | bool                     | `false`      | `true` = block until editor exits            |

### Template context

Available in `rule.group`, `editor.command`, `editor.listen`, `editor.args_*`:

| variable        | example                         |
| --------------- | ------------------------------- |
| `file_path`     | `C:/Users/you/notes/todo.md`    |
| `file_dir`      | `C:/Users/you/notes`            |
| `file_name`     | `todo.md`                       |
| `file_stem`     | `todo`                          |
| `file_ext`      | `md` (no leading dot)           |
| `editor_*`      | same five fields for `command`  |
| `cwd`           | current working directory       |
| `group`         | resolved group (phase 3 only)   |
| `rule`          | resolved rule name (phase 3)    |
| `vars.<key>`    | your `[vars]` entries           |
| `env.<KEY>`     | process env at edtr invocation  |

And these edtr-specific Tera functions:

- `is_windows()`, `is_linux()`, `is_mac()` — booleans for OS branching.

Plus everything Tera ships — `replace`, `split`, `join`, `length`, `now()`,
and all other stock [Tera features][tera].

## CLI reference

```
edtr [FILES]...            # dispatch files per rules (default action)
edtr check <FILES>...      # dry-run: show matched rule per file
edtr completion <shell>    # emit shell completion script
edtr --help
edtr --version

# v0.2+:
edtr list                  # list alive editor instances
edtr kill <group> | --all  # terminate instances
edtr config path | edit | validate | show
```

Flags:

- `-c, --config <PATH>` — override config path
- `-E, --editor <NAME>` — bypass rule, force editor
- `-G, --group <NAME>`  — bypass rule, force group
- `--dry-run`           — print the resolved plan without executing
- `-v, --verbose`       — `-v` = info, `-vv` = debug, `-vvv` = trace

Logging is also controllable via `RUST_LOG`.

## Roadmap

- **v0.1** *(this release)*: core dispatch, neovim + generic backends,
  `check`, `completion`, default config, `$EDITOR` compatibility.
- **v0.2**: `list` / `kill` / `config edit|validate|show`, `open` / `send`,
  neovim `remote + sync` via `nvim_buf_attach`.
- **v0.3+**: `script` editor kind, per-file arg placement, plugin hooks.

## Heritage

`edtr` is a Rust rewrite of [`hitori.vim`][hitori] (denops-based). The old
plugin had a slow cold start on Windows and was vim/neovim-only; `edtr` is
fast and editor-agnostic while preserving the core "one editor instance"
philosophy.

## License

[MIT](./LICENSE) — © 2026 yukimemi.

[tera]: https://keats.github.io/tera/docs/#built-ins
[hitori]: https://github.com/yukimemi/hitori.vim
