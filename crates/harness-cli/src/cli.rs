use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "harness", version, about = "Language-agnostic Agent Harness")]
pub struct Cli {
    #[arg(
        long,
        global = true,
        default_value = "harness.toml",
        value_name = "FILE"
    )]
    pub config: PathBuf,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    Config(ConfigArgs),
    Session(SessionArgs),
    Run(RunArgs),
    Inspect(InspectArgs),
}

#[derive(Debug, Args)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub command: ConfigCommand,
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    Check,
    Resolve(ResolveArgs),
}

#[derive(Debug, Args)]
pub struct ResolveArgs {
    #[arg(long)]
    pub profile: Option<String>,

    #[arg(long)]
    pub workspace: Option<String>,

    #[arg(long)]
    pub session: Option<String>,

    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct SessionArgs {
    #[command(subcommand)]
    pub command: SessionCommand,
}

#[derive(Debug, Subcommand)]
pub enum SessionCommand {
    Create,
}

#[derive(Debug, Args)]
pub struct RunArgs {
    pub session_id: String,

    #[arg(long)]
    pub profile: Option<String>,

    #[arg(long)]
    pub workspace: Option<String>,
}

#[derive(Debug, Args)]
pub struct InspectArgs {
    pub session_id: String,

    #[arg(long)]
    pub pretty: bool,
}
