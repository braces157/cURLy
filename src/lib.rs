pub mod cli;
pub mod error;
pub mod executor;
pub mod output;
pub mod request;
pub mod storage;
pub mod tui;

use clap::Parser;

use crate::{cli::Cli, error::CurlyError};

pub async fn run() -> Result<u8, CurlyError> {
    let cli = Cli::parse();
    cli::dispatch(cli).await
}
