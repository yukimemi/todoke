use anyhow::{Result, bail};
use clap::Subcommand;

#[derive(Subcommand, Debug)]
pub enum ConfigSub {
    #[command(about = "Print resolved config file path")]
    Path,
    #[command(about = "Open the config file through edtr itself")]
    Edit,
    #[command(about = "Validate TOML syntax and Tera templates")]
    Validate,
    #[command(about = "Print the loaded config")]
    Show {
        #[arg(
            long,
            help = "Show templates fully resolved using an empty file context"
        )]
        resolved: bool,
    },
}

pub async fn run(sub: ConfigSub) -> Result<()> {
    match sub {
        ConfigSub::Path => bail!("not implemented yet"),
        ConfigSub::Edit => bail!("not implemented yet"),
        ConfigSub::Validate => bail!("not implemented yet"),
        ConfigSub::Show { resolved: _ } => bail!("not implemented yet"),
    }
}
