//! Standalone entry point for the drmr browser console.

use clap::Parser;
use drmr_server::{JobCli, ServeArgs};
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "drmr-server",
    version,
    about = "The drmrr console server (Rust); configured from DRMR_* environment variables"
)]
struct Cli {
    #[command(flatten)]
    args: ServeArgs,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    drmr_server::run_server(&cli.args, JobCli::sibling(), "drmr-server")
}
