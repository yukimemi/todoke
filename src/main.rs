use anyhow::Result;
use clap::{CommandFactory, Parser};

mod cli;
mod config;
mod dispatcher;
mod editors;
mod matcher;
mod platform;
mod style;
mod template;

use cli::{Cli, Command};

#[tokio::main]
async fn main() -> Result<()> {
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
        Some(Command::Check { files }) => dispatcher::check(&cli, &files).await,
        Some(Command::Doctor) => dispatcher::doctor(&cli).await,
        Some(Command::Config(sub)) => cli::config::run(sub).await,
        Some(Command::Completion { shell }) => {
            clap_complete::generate(shell, &mut Cli::command(), "todoke", &mut std::io::stdout());
            Ok(())
        }
    }
}
