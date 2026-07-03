//! `tz` — manage managed Tenzir pipelines through the Tenzir Platform.

mod apply;
mod auth;
mod cli;
mod client;
mod config;
mod error;
mod model;
mod output;
mod project;
mod reconcile;
mod status;

use std::process::ExitCode;

use owo_colors::OwoColorize;

#[tokio::main]
async fn main() -> ExitCode {
    match cli::run().await {
        Ok(code) => ExitCode::from(code),
        Err(hinted) => {
            eprintln!("{}: {}", "error".red().bold(), hinted.error);
            for hint in &hinted.hints {
                eprintln!("{}: {hint}", "hint".cyan().bold());
            }
            ExitCode::FAILURE
        }
    }
}
