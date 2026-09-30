//! CryptUI binary entry point.
//!
//! Owns argument parsing, logging setup, and — from phase P2 on — terminal
//! initialisation and panic-safe restore. It also exposes the headless entry
//! points that let the read path be verified without a TTY.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, ValueEnum};
use tracing_subscriber::EnvFilter;

use cryptui::config::{self, Account, AccountMode, Config};
use cryptui::venue::binance_futures::BinanceFutures;
use cryptui::venue::{Interval, Venue, VenueError, VenueId};

/// Terminal UI for monitoring crypto exchange accounts (read-only).
#[derive(Debug, Parser)]
#[command(name = "cryptui", version, about, long_about = None)]
struct Cli {
    /// Configuration file to use.
    ///
    /// Defaults to $CRYPTUI_CONFIG, then ~/.config/cryptui/config.toml.
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,

    /// Account to read from; defaults to the configured `default_account`.
    #[arg(long, value_name = "NAME")]
    account: Option<String>,

    /// Load the configuration, print a credential-free summary, and exit.
    #[arg(long)]
    print_config: bool,

    /// Fetch one data set from the venue, print it, and exit.
    #[arg(long, value_enum, value_name = "WHAT")]
    print: Option<PrintKind>,

    /// Symbol used by `--print klines`.
    #[arg(long, value_name = "SYMBOL")]
    symbol: Option<String>,

    /// Candle interval used by `--print klines`.
    #[arg(long, value_name = "INTERVAL", default_value = "15m")]
    interval: Interval,

    /// Row limit for `--print klines` and `--print symbols`.
    #[arg(long, value_name = "N", default_value_t = 20)]
    limit: u32,

    /// Increase log verbosity; repeat for more.
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,
}

/// Data sets that can be printed without starting the TUI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum PrintKind {
    /// Tradable contracts.
    Symbols,
    /// Open positions.
    Positions,
    /// Balances and account totals.
    Balances,
    /// Candles for a symbol.
    Klines,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    init_tracing(cli.verbose);

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

    if let Some(kind) = cli.print {
        return match kind {
            PrintKind::Symbols => print_symbols(&cli, &config).await,
            PrintKind::Positions => print_positions(&cli, &config).await,
            PrintKind::Balances => print_balances(&cli, &config).await,
            PrintKind::Klines => print_klines(&cli, &config).await,
        };
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

/// Build the venue client for one configured account.
fn venue_for(name: &str, account: &Account) -> Result<Box<dyn Venue>, VenueError> {
    match account.mode(name) {
        Ok(AccountMode::Api { testnet }) => match account.venue() {
            VenueId::BinanceFutures => {
                let api_key = account.api_key().cloned().ok_or_else(|| {
                    VenueError::Credentials(format!("account `{name}` has no `api_key`"))
                })?;
                let api_secret = account.api_secret().cloned().ok_or_else(|| {
                    VenueError::Credentials(format!("account `{name}` has no `api_secret`"))
                })?;
                Ok(Box::new(BinanceFutures::new(api_key, api_secret, testnet)?))
            }
        },
        Ok(AccountMode::Fixture { path }) => Err(VenueError::FixtureUnsupported { path }),
        Err(error) => Err(VenueError::Credentials(error.to_string())),
    }
}

/// Resolve the account to read from, connect, and synchronise the clock.
async fn connect(cli: &Cli, config: &Config) -> Result<(String, Box<dyn Venue>), String> {
    let name = cli
        .account
        .clone()
        .unwrap_or_else(|| config.default_account().to_owned());
    let account = config
        .account(&name)
        .ok_or_else(|| format!("no account named `{name}` in the configuration"))?;

    let venue = venue_for(&name, account).map_err(|error| error.to_string())?;
    venue
        .sync()
        .await
        .map_err(|error| format!("{name}: {error}"))?;

    Ok((name, venue))
}

/// `--print symbols`
async fn print_symbols(cli: &Cli, config: &Config) -> ExitCode {
    let (_name, venue) = match connect(cli, config).await {
        Ok(pair) => pair,
        Err(error) => return fail(error),
    };
    let symbols = match venue.symbols().await {
        Ok(symbols) => symbols,
        Err(error) => return fail(error),
    };

    let shown = cli.limit as usize;
    println!("{} tradable contracts on {}", symbols.len(), venue.id());
    for symbol in symbols.iter().take(shown) {
        println!(
            "  {:<16} {}/{}",
            symbol.name, symbol.base_asset, symbol.quote_asset
        );
    }
    if symbols.len() > shown {
        println!("  … {} more", symbols.len() - shown);
    }

    ExitCode::SUCCESS
}

/// `--print positions`
async fn print_positions(cli: &Cli, config: &Config) -> ExitCode {
    let (name, venue) = match connect(cli, config).await {
        Ok(pair) => pair,
        Err(error) => return fail(error),
    };
    let positions = match venue.positions().await {
        Ok(positions) => positions,
        Err(error) => return fail(error),
    };

    println!("{name}: {} open position(s)", positions.len());
    println!(
        "  {:<16} {:>5} {:>14} {:>12} {:>12} {:>12} {:>12}",
        "symbol", "side", "size", "entry", "mark", "margin", "pnl"
    );

    let mut total_pnl = 0.0;
    for position in &positions {
        total_pnl += position.unrealized_pnl;
        println!(
            "  {:<16} {:>5} {:>14} {:>12} {:>12} {:>12} {:>12}",
            position.symbol,
            position.side,
            compact(position.size),
            compact(position.entry_price),
            compact(position.mark_price),
            compact(position.initial_margin),
            compact(position.unrealized_pnl)
        );
    }
    println!("  total unrealized PnL {}", compact(total_pnl));

    ExitCode::SUCCESS
}

/// `--print balances`
async fn print_balances(cli: &Cli, config: &Config) -> ExitCode {
    let (name, venue) = match connect(cli, config).await {
        Ok(pair) => pair,
        Err(error) => return fail(error),
    };
    let snapshot = match venue.account().await {
        Ok(snapshot) => snapshot,
        Err(error) => return fail(error),
    };

    println!("{name} on {}", venue.id());
    println!(
        "  wallet {}  equity {}  unrealized {}  available {}  initial margin {}  maintenance margin {}",
        compact(snapshot.wallet_balance),
        compact(snapshot.equity),
        compact(snapshot.unrealized_pnl),
        compact(snapshot.available_balance),
        compact(snapshot.initial_margin),
        compact(snapshot.maintenance_margin)
    );
    for balance in &snapshot.balances {
        println!(
            "  {:<8} total {:>16}   available {:>16}",
            balance.asset,
            compact(balance.total),
            compact(balance.available)
        );
    }

    ExitCode::SUCCESS
}

/// `--print klines`
async fn print_klines(cli: &Cli, config: &Config) -> ExitCode {
    let Some(symbol) = cli.symbol.clone() else {
        return fail("--print klines needs --symbol <SYMBOL>");
    };

    let (_name, venue) = match connect(cli, config).await {
        Ok(pair) => pair,
        Err(error) => return fail(error),
    };
    let klines = match venue.klines(&symbol, cli.interval, cli.limit).await {
        Ok(klines) => klines,
        Err(error) => return fail(error),
    };

    println!(
        "{} {} — {} candle(s) from {}",
        symbol,
        cli.interval,
        klines.len(),
        venue.id()
    );
    for kline in &klines {
        let state = if kline.closed { "" } else { "  (forming)" };
        println!(
            "  {}  O {:>12}  H {:>12}  L {:>12}  C {:>12}  V {:>14}{state}",
            format_time(kline.open_time_ms),
            compact(kline.open),
            compact(kline.high),
            compact(kline.low),
            compact(kline.close),
            compact(kline.volume)
        );
    }

    ExitCode::SUCCESS
}

/// Format a millisecond timestamp as UTC `YYYY-MM-DD HH:MM`.
fn format_time(milliseconds: i64) -> String {
    let format = time::macros::format_description!("[year]-[month]-[day] [hour]:[minute]");
    time::OffsetDateTime::from_unix_timestamp(milliseconds / 1_000)
        .ok()
        .and_then(|stamp| stamp.format(&format).ok())
        .unwrap_or_else(|| milliseconds.to_string())
}

/// Format a number with at most four decimals, without trailing zeros.
fn compact(value: f64) -> String {
    let text = format!("{value:.4}");
    if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.').to_owned()
    } else {
        text
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
fn init_tracing(verbosity: u8) {
    let default = match verbosity {
        0 => "info",
        1 => "debug",
        _ => "trace",
    };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}
