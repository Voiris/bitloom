mod cli;
mod commands;
mod execution;
mod logging;
mod output;
mod project;

use std::process::ExitCode;

use clap::Parser;

use crate::cli::{Cli, Command};

#[tokio::main]
async fn main() -> ExitCode {
    let result = match Cli::parse().command {
        Command::New(args) => commands::new::run(args).await,
        Command::Build(args) => commands::build::run(args).await,
        Command::Clean(args) => commands::clean::run(args).await,
        Command::Flash(args) => commands::flash::run(args).await,
        Command::Monitor(args) => commands::monitor::run(args).await,
        Command::Run(args) => commands::run::run(args).await,
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            output::error(format_args!("{error:#}"));
            ExitCode::FAILURE
        }
    }
}
