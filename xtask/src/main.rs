use clap::Parser;
use swift::SwiftCommand;

mod swift;
mod utils;
mod workspace;

#[derive(Parser)]
#[command(name = "xtask")]
#[command(bin_name = "cargo xtask")]
enum Cli {
    #[command(subcommand)]
    Swift(SwiftCommand),
}

fn main() -> Result<(), anyhow::Error> {
    match Cli::parse() {
        Cli::Swift(cmd) => cmd.run(),
    }
}
