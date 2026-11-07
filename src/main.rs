#[cfg(target_os = "macos")]
embed_plist::embed_info_plist!("../Info.plist");

mod catalog;
mod cli;
mod commands;
mod dates;
mod emlx;
mod meeting;
mod output;
mod permissions;
mod responsibility;
mod scripting;
mod store;
mod version;

use anyhow::{Context, Result};
use clap::Parser;
use cli::{Cli, Command};
use std::io::Write;
use std::process::ExitCode;

fn main() -> Result<ExitCode> {
    if let Some(code) = responsibility::reexec_disclaimed() {
        return Ok(code);
    }

    let Cli { json, command } = Cli::parse();
    if let Command::Completions { shell } = command {
        cli::print_completions(shell);
        return Ok(ExitCode::SUCCESS);
    }

    let output = commands::run(command)?;
    let mut stdout = std::io::stdout().lock();
    if json {
        output
            .write_json(&mut stdout)
            .context("failed to write JSON output")?;
        writeln!(stdout)?;
    } else {
        output.write_human(&mut stdout)?;
    }
    stdout.flush()?;

    Ok(if output.failed() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}
