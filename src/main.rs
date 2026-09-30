//! CryptUI binary entry point.
//!
//! Owns argument parsing, logging setup, and terminal lifetime. Headless entry
//! points (`--print-config`, `--print …`, `--dump-frame`) exist so every layer
//! can be verified without an interactive terminal.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, ValueEnum};
use tracing_subscriber::EnvFilter;

use cryptui::app::{self, RunOptions};
use cryptui::config::{self, Account, AccountMode, Config};
use cryptui::state::{App, Update};
use cryptui::ui::{self, format};
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

    /// Render a single frame as ANSI on stdout and exit.
    ///
    /// Writes escape sequences, so redirect it: `cryptui --dump-frame > frame.ans`.
    #[arg(long)]
    dump_frame: bool,

    /// Frame width for `--dump-frame`.
    #[arg(long, value_name = "COLUMNS", default_value_t = 120)]
    width: u16,

    /// Frame height for `--dump-frame`.
    #[arg(long, value_name = "ROWS", default_value_t = 40)]
    height: u16,

    /// Symbol used by `--print klines`.
    #[arg(long, value_name = "SYMBOL")]
    symbol: Option<String>,

    /// Candle interval; defaults to `settings.default_interval` from the config.
    #[arg(long, value_name = "INTERVAL")]
    interval: Option<Interval>,

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

/// A configured account with a live client attached.
struct Connected {
    name: String,
    label: String,
    venue: Box<dyn Venue>,
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

    let connected = match connect(&cli, &config).await {
        Ok(connected) => connected,
        Err(error) => return fail(error),
    };

    if cli.dump_frame {
        return dump_frame(&cli, &config, connected).await;
    }

    tracing::debug!(config = %path.display(), account = %connected.name, "starting the TUI");
    let options = RunOptions {
        account_label: connected.label,
        venue: connected.venue.id(),
        interval: effective_interval(&cli, &config),
        refresh_interval_ms: config.settings().refresh_interval_ms(),
    };

    match app::run(connected.venue, options) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => fail(error),
    }
}

/// The interval to open the chart with: the command line wins over the config.
fn effective_interval(cli: &Cli, config: &Config) -> Interval {
    cli.interval
        .unwrap_or_else(|| config.settings().default_interval())
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
async fn connect(cli: &Cli, config: &Config) -> Result<Connected, String> {
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

    Ok(Connected {
        name,
        label: account.label().to_owned(),
        venue,
    })
}

/// `--dump-frame`: fetch real data, render one frame, and exit.
async fn dump_frame(cli: &Cli, config: &Config, connected: Connected) -> ExitCode {
    let positions = match connected.venue.positions().await {
        Ok(positions) => positions,
        Err(error) => return fail(error),
    };
    let account = match connected.venue.account().await {
        Ok(account) => account,
        Err(error) => return fail(error),
    };

    let mut state = App::new(
        connected.label,
        connected.venue.id(),
        effective_interval(cli, config),
        config.settings().refresh_interval_ms(),
    );
    // Go through the update path rather than the setters, so the header reports
    // feed health exactly as it will during a live session.
    state.apply(Update::Positions(positions));
    state.apply(Update::Account(Box::new(account)));

    match ui::dump_frame(&state, cli.width, cli.height) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => fail(error),
    }
}

/// `--print symbols`
async fn print_symbols(cli: &Cli, config: &Config) -> ExitCode {
    let connected = match connect(cli, config).await {
        Ok(connected) => connected,
        Err(error) => return fail(error),
    };
    let symbols = match connected.venue.symbols().await {
        Ok(symbols) => symbols,
        Err(error) => return fail(error),
    };

    let shown = cli.limit as usize;
    println!(
        "{} tradable contracts on {}",
        symbols.len(),
        connected.venue.id()
    );
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
    let connected = match connect(cli, config).await {
        Ok(connected) => connected,
        Err(error) => return fail(error),
    };
    let positions = match connected.venue.positions().await {
        Ok(positions) => positions,
        Err(error) => return fail(error),
    };

    println!("{}: {} open position(s)", connected.name, positions.len());
    println!(
        "  {:<16} {:>5} {:>14} {:>12} {:>12} {:>12} {:>16}",
        "symbol", "side", "size", "entry", "mark", "margin", "pnl"
    );

    let mut total_pnl = 0.0;
    for position in &positions {
        total_pnl += position.unrealized_pnl;
        println!(
            "  {:<16} {:>5} {:>14} {:>12} {:>12} {:>12} {:>16}",
            position.symbol,
            position.side,
            format::quantity(position.size),
            format::price(position.entry_price),
            format::price(position.mark_price),
            format::money(position.initial_margin),
            format::signed_money(position.unrealized_pnl)
        );
    }
    println!("  total unrealized PnL {}", format::signed_money(total_pnl));

    ExitCode::SUCCESS
}

/// `--print balances`
async fn print_balances(cli: &Cli, config: &Config) -> ExitCode {
    let connected = match connect(cli, config).await {
        Ok(connected) => connected,
        Err(error) => return fail(error),
    };
    let snapshot = match connected.venue.account().await {
        Ok(snapshot) => snapshot,
        Err(error) => return fail(error),
    };

    println!("{} on {}", connected.name, connected.venue.id());
    println!(
        "  wallet {}  equity {}  unrealized {}  available {}  initial margin {}  maintenance margin {}",
        format::money(snapshot.wallet_balance),
        format::money(snapshot.equity),
        format::signed_money(snapshot.unrealized_pnl),
        format::money(snapshot.available_balance),
        format::money(snapshot.initial_margin),
        format::money(snapshot.maintenance_margin)
    );
    for balance in &snapshot.balances {
        println!(
            "  {:<8} total {:>16}   available {:>16}",
            balance.asset,
            format::money(balance.total),
            format::money(balance.available)
        );
    }

    ExitCode::SUCCESS
}

/// `--print klines`
async fn print_klines(cli: &Cli, config: &Config) -> ExitCode {
    let Some(symbol) = cli.symbol.clone() else {
        return fail("--print klines needs --symbol <SYMBOL>");
    };

    let connected = match connect(cli, config).await {
        Ok(connected) => connected,
        Err(error) => return fail(error),
    };
    let interval = effective_interval(cli, config);
    let klines = match connected.venue.klines(&symbol, interval, cli.limit).await {
        Ok(klines) => klines,
        Err(error) => return fail(error),
    };

    println!(
        "{} {} — {} candle(s) from {}",
        symbol,
        interval,
        klines.len(),
        connected.venue.id()
    );
    for kline in &klines {
        let state = if kline.closed { "" } else { "  (forming)" };
        println!(
            "  {}  O {:>12}  H {:>12}  L {:>12}  C {:>12}  V {:>16}{state}",
            format_time(kline.open_time_ms),
            format::price(kline.open),
            format::price(kline.high),
            format::price(kline.low),
            format::price(kline.close),
            format::quantity(kline.volume)
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

/// Report a startup failure on stderr and exit non-zero.
fn fail(error: impl std::fmt::Display) -> ExitCode {
    eprintln!("cryptui: {error}");
    ExitCode::FAILURE
}

/// Send diagnostics to a log file-friendly stderr.
///
/// The TUI owns the screen while it runs, so nothing is logged at the default
/// level during a session; `-v` is for troubleshooting outside the alternate
/// screen.
fn init_tracing(verbosity: u8) {
    let default = match verbosity {
        0 => "warn",
        1 => "info",
        2 => "debug",
        _ => "trace",
    };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}

#[cfg(test)]
mod tests {
    use super::{Cli, effective_interval};
    use clap::Parser;
    use cryptui::config::Config;
    use cryptui::venue::Interval;

    fn config() -> Config {
        // `load_with` resolves `${VAR}` references without touching the process
        // environment, which keeps this test independent of the real config.
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("config.template.toml");
        Config::load_with(&path, &|_name| Some("test-value".to_owned())).expect("config loads")
    }

    #[test]
    fn the_interval_defaults_to_the_configured_value() {
        let cli = Cli::parse_from(["cryptui"]);
        assert_eq!(cli.interval, None);
        assert_eq!(effective_interval(&cli, &config()), Interval::M15);
    }

    #[test]
    fn the_command_line_interval_wins() {
        let cli = Cli::parse_from(["cryptui", "--interval", "4h"]);
        assert_eq!(cli.interval, Some(Interval::H4));
        assert_eq!(effective_interval(&cli, &config()), Interval::H4);
    }

    #[test]
    fn dump_frame_defaults_to_a_usable_viewport() {
        let cli = Cli::parse_from(["cryptui", "--dump-frame"]);
        assert!(cli.dump_frame);
        assert_eq!((cli.width, cli.height), (120, 40));
    }

    #[test]
    fn an_unknown_interval_is_rejected_with_the_allowed_values() {
        let error = Cli::try_parse_from(["cryptui", "--interval", "7m"]).unwrap_err();
        let rendered = error.to_string();
        assert!(rendered.contains("7m"), "got: {rendered}");
        assert!(
            rendered.contains("15m"),
            "allowed values listed: {rendered}"
        );
    }
}
