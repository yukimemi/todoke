// Release builds on Windows use the "windows" subsystem so launching
// todoke from explorer / shortcut / file association doesn't flash a
// transient console window. Debug builds stay on the default "console"
// subsystem to keep dev ergonomics unchanged.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use anyhow::Result;
use clap::{CommandFactory, Parser};

mod backends;
mod cli;
mod config;
#[cfg(windows)]
mod console_attach;
mod dispatcher;
mod input;
mod matcher;
mod platform;
mod style;
mod template;

use cli::{Cli, Command};

#[tokio::main]
async fn main() -> Result<()> {
    // MUST be the very first thing in main().
    //
    // Rust's io::stdout / io::stderr / io::stdin cache their OS handle
    // on first use. Any stdio access before this runs would freeze the
    // null handle in place and the SetStdHandle calls below would
    // become no-ops for Rust's stdio. Keep this at the top; don't
    // introduce earlier stdio writers (panic hooks, static init, etc.)
    // without rechecking this invariant.
    //
    // On Windows, attach to the parent terminal (if any) so logs and
    // stdout reach whoever launched us from PowerShell / cmd. When
    // launched from explorer there is no parent console — the call
    // is a no-op and stdio sinks silently.
    #[cfg(windows)]
    console_attach::attach_parent_console();

    let mut cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| cli.log_level().into()),
        )
        .with_writer(std::io::stderr)
        .init();

    match cli.command.take() {
        None => dispatcher::dispatch(&cli, &cli.files).await,
        Some(Command::List { alive_only }) => dispatcher::list(alive_only).await,
        Some(Command::Kill { group, all }) => dispatcher::kill(group.as_deref(), all).await,
        Some(Command::Check { inputs }) => dispatcher::check(&cli, &inputs).await,
        Some(Command::Doctor) => dispatcher::doctor(&cli).await,
        Some(Command::Config(sub)) => cli::config::run(sub, cli.config.as_deref()).await,
        Some(Command::Completion { shell }) => {
            clap_complete::generate(shell, &mut Cli::command(), "todoke", &mut std::io::stdout());
            Ok(())
        }
    }
}
