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
    #[arg(
        value_name = "INPUTS",
        help = "Files, URLs, or raw strings to dispatch (no subcommand = default dispatch)"
    )]
    pub files: Vec<PathBuf>,

    #[arg(
        short = 'c',
        long = "config",
        value_name = "PATH",
        help = "Override config path",
        global = true
    )]
    pub config: Option<PathBuf>,

    #[arg(
        short = 'E',
        long = "editor",
        value_name = "NAME",
        help = "Bypass rules, force handler"
    )]
    pub editor: Option<String>,

    #[arg(
        short = 'G',
        long = "group",
        value_name = "NAME",
        help = "Bypass rules, force group"
    )]
    pub group: Option<String>,

    #[arg(
        long = "as",
        value_name = "KIND",
        value_enum,
        help = "Force how each argument is classified (skip auto-detection)"
    )]
    pub as_kind: Option<InputKind>,

    #[arg(
        long = "dry-run",
        help = "Resolve rules and log decisions without dispatching"
    )]
    pub dry_run: bool,

    #[arg(short = 'v', long = "verbose", action = clap::ArgAction::Count, help = "Increase log verbosity (-v, -vv)")]
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
    },

    #[command(about = "Dry-run: show which rule matches each file without dispatching")]
    Check {
        #[arg(value_name = "FILES", required = true)]
        files: Vec<PathBuf>,
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
