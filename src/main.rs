//! CryptUI binary entry point.
//!
//! Owns argument parsing, logging setup, and — from phase P2 on — terminal
//! initialisation and panic-safe restore. Phase 0 only proves the wiring
//! builds and runs; no TUI or network code exists yet.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use tracing_subscriber::EnvFilter;

/// Terminal UI for monitoring crypto exchange accounts (read-only).
#[derive(Debug, Parser)]
#[command(name = "cryptui", version, about, long_about = None)]
struct Cli {
    /// Configuration file to use.
    ///
    /// Defaults to $CRYPTUI_CONFIG, then ~/.config/cryptui/config.toml.
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    init_tracing();

    tracing::debug!(config = ?cli.config, "startup");

    eprintln!(
        "cryptui {} — read-only account monitoring. \
         Skeleton only: the TUI arrives in phase P2.",
        cryptui::VERSION
    );

    ExitCode::SUCCESS
}

/// Send diagnostics to stderr.
///
/// Once the TUI owns the screen (phase P2) this switches to a log file, because
/// stdout/stderr writes would corrupt the rendered frame.
fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}
