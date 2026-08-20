mod cli;
mod commands;
mod error;
mod identity;
mod observability;

use clap::Parser as _;

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    if let Err(error) = commands::execute(cli::Cli::parse()).await {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}
