use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "wakaru")]
#[command(about = "Rust prototype for Wakaru migration")]
pub struct Cli {
    #[arg(value_name = "inputs")]
    pub inputs: Vec<String>,

    #[arg(short, long, default_value = "./out/")]
    pub output: String,

    #[arg(long)]
    pub unpacker_output: Option<String>,

    #[arg(long)]
    pub unminify_output: Option<String>,

    #[arg(long)]
    pub perf_output: Option<String>,

    #[arg(short, long)]
    pub force: bool,

    #[arg(long, default_value_t = 1)]
    pub concurrency: usize,

    #[arg(long)]
    pub perf: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    All {
        #[arg(value_name = "inputs")]
        inputs: Vec<String>,
    },
    #[command(alias = "unpack")]
    Unpacker {
        #[arg(value_name = "inputs")]
        inputs: Vec<String>,
    },
    Unminify {
        #[arg(value_name = "inputs")]
        inputs: Vec<String>,
    },
}
