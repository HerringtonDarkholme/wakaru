mod args;
mod commands;
mod interactive;
mod output;
mod path;
mod perf;

use anyhow::Result;
use clap::Parser;

use crate::args::Cli;

fn main() -> Result<()> {
    commands::run(Cli::parse())
}
