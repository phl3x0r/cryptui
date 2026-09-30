//! CryptUI binary entry point.
//!
//! Owns argument parsing, logging setup, and terminal lifetime. Headless entry
//! points (`--print-config`, `--print …`, `--dump-frame`) exist so every layer
//! can be verified without an interactive terminal.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, ValueEnum};

use cryptui::accounts::{self, AccountHandle};
use cryptui::app::{self, RunOptions};
use cryptui::config::{self, Config};
use cryptui::logging::{self, Sink};
use cryptui::state::{App, Update};
use cryptui::ui::{self, format};
use cryptui::venue::{Interval, Venue};

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

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();

    // The interactive UI owns the screen, so its diagnostics go to a file:
    // a log line written to stderr lands on top of the frame and stays there,
    // because ratatui only repaints cells that change.
    let interactive = cli.print.is_none() && !cli.print_config && !cli.dump_frame;
    let sink = if interactive {
        Sink::File
    } else {
        Sink::Stderr
    };
    let log_file = logging::init(cli.verbose, sink);

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

    if cli.dump_frame {
        let (label, venue) = match open(&cli, &config).await {
            Ok(opened) => opened,
            Err(error) => return fail(error),
        };
        return dump_frame(&cli, &config, label, venue).await;
    }

    // Interactive session: hand every configured account to the event loop so it
    // can switch between them without re-reading the configuration.
    let handles = match accounts::handles(&config) {
        Ok(handles) => handles,
        Err(error) => return fail(error),
    };
    if handles.is_empty() {
        return fail("no accounts configured");
    }

    // `--account` selects the opening account, exactly as it does for the
    // headless commands; an unknown name is an error rather than a silent
    // fallback to the configured default.
    let active = match select_account(&cli, &config, &handles) {
        Ok(handle) => handles
            .iter()
            .position(|candidate| candidate.name() == handle.name())
            .unwrap_or_default(),
        Err(error) => return fail(error),
    };

    let options = RunOptions {
        active,
        accounts: handles,
        interval: effective_interval(&cli, &config),
        refresh_interval_ms: config.settings().refresh_interval_ms(),
        chart_history: config.settings().chart_history_candles(),
    };

    tracing::debug!(
        config = %path.display(),
        accounts = options.accounts.len(),
        log = ?log_file,
        "starting the TUI"
    );
    match app::run(options) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => fail(error),
    }
}

/// The interval to open the chart with: the command line wins over the config.
fn effective_interval(cli: &Cli, config: &Config) -> Interval {
    cli.interval
        .unwrap_or_else(|| config.settings().default_interval())
}

/// The account named on the command line, or the configured default.
fn select_account<'a>(
    cli: &Cli,
    config: &Config,
    handles: &'a [AccountHandle],
) -> Result<&'a AccountHandle, String> {
    match &cli.account {
        Some(name) => handles
            .iter()
            .find(|handle| handle.name() == name)
            .ok_or_else(|| format!("no account named `{name}` in the configuration")),
        None => {
            let index = accounts::default_index(config, handles);
            handles
                .get(index)
                .ok_or_else(|| "no accounts configured".to_owned())
        }
    }
}

/// Connect the selected account and synchronise its clock.
///
/// Returns the label the UI shows and the ready client.
async fn open(cli: &Cli, config: &Config) -> Result<(String, Box<dyn Venue>), String> {
    let handles = accounts::handles(config).map_err(|error| error.to_string())?;
    let handle = select_account(cli, config, &handles)?;

    let venue = handle.connect().map_err(|error| error.to_string())?;
    venue
        .sync()
        .await
        .map_err(|error| format!("{}: {error}", handle.name()))?;

    Ok((handle.label().to_owned(), venue))
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

/// `--dump-frame`: fetch real data, render one frame, and exit.
async fn dump_frame(cli: &Cli, config: &Config, label: String, venue: Box<dyn Venue>) -> ExitCode {
    let positions = match venue.positions().await {
        Ok(positions) => positions,
        Err(error) => return fail(error),
    };
    let account = match venue.account().await {
        Ok(account) => account,
        Err(error) => return fail(error),
    };

    let mut state = App::new(
        label,
        venue.id(),
        effective_interval(cli, config),
        config.settings().refresh_interval_ms(),
    );
    // Go through the update path rather than the setters, so the header reports
    // feed health exactly as it will during a live session.
    state.apply(Update::Positions(positions));
    state.apply(Update::Account(Box::new(account)));

    // Load the chart target so the dump shows a real chart rather than a
    // placeholder pane.
    if let Some(symbol) = state.effective_symbol().map(str::to_owned) {
        state.focus_chart(symbol.clone());
        let interval = state.chart_interval();
        match venue
            .klines(&symbol, interval, config.settings().chart_history_candles())
            .await
        {
            Ok(candles) => state.apply(Update::History {
                symbol,
                interval,
                candles,
            }),
            Err(error) => return fail(error),
        }
    }

    match ui::dump_frame(&state, cli.width, cli.height) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => fail(error),
    }
}

/// `--print symbols`
async fn print_symbols(cli: &Cli, config: &Config) -> ExitCode {
    let (_, venue) = match open(cli, config).await {
        Ok(opened) => opened,
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
    let (label, venue) = match open(cli, config).await {
        Ok(opened) => opened,
        Err(error) => return fail(error),
    };
    let positions = match venue.positions().await {
        Ok(positions) => positions,
        Err(error) => return fail(error),
    };

    println!("{label}: {} open position(s)", positions.len());
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
    let (label, venue) = match open(cli, config).await {
        Ok(opened) => opened,
        Err(error) => return fail(error),
    };
    let snapshot = match venue.account().await {
        Ok(snapshot) => snapshot,
        Err(error) => return fail(error),
    };

    println!("{label} on {}", venue.id());
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

    let (_, venue) = match open(cli, config).await {
        Ok(opened) => opened,
        Err(error) => return fail(error),
    };
    let interval = effective_interval(cli, config);
    let klines = match venue.klines(&symbol, interval, cli.limit).await {
        Ok(klines) => klines,
        Err(error) => return fail(error),
    };

    println!(
        "{} {} — {} candle(s) from {}",
        symbol,
        interval,
        klines.len(),
        venue.id()
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
