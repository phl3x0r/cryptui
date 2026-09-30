//! CryptUI binary entry point.
//!
//! Owns argument parsing, logging setup, and — from phase P2 on — terminal
//! initialisation and panic-safe restore. It also exposes the headless entry
//! points that let each layer be verified without a TTY.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;
use tracing_subscriber::EnvFilter;

use cryptui::config::{self, Config};

/// Terminal UI for monitoring crypto exchange accounts (read-only).
#[derive(Debug, Parser)]
#[command(name = "cryptui", version, about, long_about = None)]
struct Cli {
    /// Configuration file to use.
    ///
    /// Defaults to $CRYPTUI_CONFIG, then ~/.config/cryptui/config.toml.
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,

    /// Load the configuration, print a credential-free summary, and exit.
    #[arg(long)]
    print_config: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    init_tracing();

    let path = match config::discover(cli.config.as_deref()) {
        Ok(path) => path,
        Err(error) => return fail(error),
    };

    let config = match Config::load(&path) {
        Ok(config) => config,
        Err(error) => return fail(error),
    };

    // Only worth warning about when the file actually holds credential material:
    // a template full of `${VAR}` references is harmless.
    if config.holds_literal_credentials()
        && let Some(warning) = config::permissions_warning(&path)
    {
        tracing::warn!("{warning}");
    }

    if cli.print_config {
        print_summary(&path, &config);
        return ExitCode::SUCCESS;
    }

    tracing::debug!(config = %path.display(), "configuration loaded");
    eprintln!(
        "cryptui {} — read-only account monitoring. \
         Loaded {} account(s); the TUI arrives in phase P2.",
        cryptui::VERSION,
        config.accounts().len()
    );

    ExitCode::SUCCESS
}

/// Print the effective configuration. Credentials are represented by
/// [`cryptui::config::Secret`], whose `Debug`/`Display` never reveal them.
fn print_summary(path: &Path, config: &Config) {
    println!("configuration    {}", path.display());
    println!("default account  {}", config.default_account());
    println!(
        "settings         refresh {} ms, interval {}, history {} candles",
        config.settings().refresh_interval_ms(),
        config.settings().default_interval(),
        config.settings().chart_history_candles()
    );
    for (name, account) in config.accounts() {
        let marker = if name == config.default_account() {
            '*'
        } else {
            ' '
        };
        println!(
            "{marker} {name:<12} {} [{}, {}]",
            account.label(),
            account.venue(),
            account.describe_mode(name)
        );
    }
}

/// Report a startup failure on stderr and exit non-zero.
fn fail(error: impl std::fmt::Display) -> ExitCode {
    eprintln!("cryptui: {error}");
    ExitCode::FAILURE
}

/// Send diagnostics to stderr.
///
/// Once the TUI owns the screen (phase P2) this switches to a log file, because
/// stdout or stderr writes would corrupt the rendered frame.
fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}
