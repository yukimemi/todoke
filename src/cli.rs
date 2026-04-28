use std::path::PathBuf;

use clap::{Parser, Subcommand};
use clap_complete::Shell;

use crate::input::InputKind;

pub mod config;

#[derive(Parser, Debug)]
#[command(
    name = "todoke",
    version,
    about = "A rule-driven dispatcher: hands incoming files, URLs, or raw strings to the right handler based on TOML-defined rules.",
    long_about = None,
)]
pub struct Cli {
    // `trailing_var_arg` is load-bearing: on some platforms (Windows CI)
    // clap rejects unknown short flags like `-f` even with
    // `allow_hyphen_values = true` — the flag parser fires first and the
    // value never reaches the positional collector. `trailing_var_arg`
    // forces everything after the first positional into this Vec, so
    // `$EDITOR=todoke`-style invocations (`todoke -c :set ft=md +42 file.txt`)
    // flow through to whichever passthrough / normal rule matches.
    //
    // Trade-off: todoke's own flags must precede inputs.
    // `todoke --todoke-dry-run +42 file.txt` works; the reverse order
    // `todoke +42 file.txt --todoke-dry-run` treats the trailing flag
    // as a positional. That's the right shape for $EDITOR callers
    // (which never inject todoke flags after inputs) and for the
    // Quick-start idioms in the README.
    #[arg(
        value_name = "INPUTS",
        help = "Files, URLs, or raw strings to dispatch (no subcommand = default dispatch)",
        trailing_var_arg = true,
        allow_hyphen_values = true
    )]
    pub files: Vec<PathBuf>,

    #[arg(
        long = "todoke-config",
        value_name = "PATH",
        help = "Override config path",
        global = true
    )]
    pub config: Option<PathBuf>,

    #[arg(
        long = "todoke-to",
        value_name = "NAME",
        help = "Bypass rules, force the target (entry under [todoke.<name>])"
    )]
    pub to: Option<String>,

    #[arg(
        long = "todoke-group",
        value_name = "NAME",
        help = "Bypass rules, force group"
    )]
    pub group: Option<String>,

    #[arg(
        long = "todoke-as",
        value_name = "KIND",
        value_enum,
        help = "Force how each argument is classified (skip auto-detection)"
    )]
    pub as_kind: Option<InputKind>,

    #[arg(
        long = "todoke-verbose",
        action = clap::ArgAction::Count,
        help = "Increase log verbosity (repeat for more: --todoke-verbose --todoke-verbose)",
    )]
    pub verbose: u8,

    #[command(subcommand)]
    pub command: Option<Command>,
}

impl Cli {
    pub fn log_level(&self) -> &'static str {
        match self.verbose {
            0 => "warn",
            1 => "info",
            2 => "debug",
            _ => "trace",
        }
    }
}

#[derive(Subcommand, Debug)]
pub enum Command {
    #[command(about = "List alive editor instances")]
    List {
        #[arg(long = "alive-only", help = "Only show instances that respond to ping")]
        alive_only: bool,
    },

    #[command(about = "Terminate editor instance(s) by group")]
    Kill {
        #[arg(value_name = "GROUP", conflicts_with = "all")]
        group: Option<String>,
        #[arg(
            long = "all",
            conflicts_with = "group",
            help = "Kill every known instance"
        )]
        all: bool,
        #[arg(
            long = "force",
            help = "Escalate to OS-level kill (SIGKILL / TerminateProcess) when `:qall!` doesn't take effect"
        )]
        force: bool,
    },

    #[command(about = "Dry-run: show the dispatch plan for the given inputs without executing")]
    Check {
        // Same shape as the top-level positional: hyphen-shaped argv
        // (`-f`, `+42`, `-sfoo`, …) must flow through as inputs without
        // clap treating them as flags to `check` itself.
        #[arg(
            value_name = "INPUTS",
            trailing_var_arg = true,
            allow_hyphen_values = true
        )]
        inputs: Vec<PathBuf>,
    },

    #[command(
        about = "Inspect the config for common footguns (unreachable rules, uncovered paths, …)"
    )]
    Doctor,

    #[command(subcommand, about = "Inspect or edit the config file")]
    Config(config::ConfigSub),

    #[command(about = "Generate shell completion script")]
    Completion {
        #[arg(value_enum)]
        shell: Shell,
    },
}
