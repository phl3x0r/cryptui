# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project uses
[semantic versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- The price axis marks the entry price of the position being charted, next to
  the live price, and only while that price is on screen. It follows the `p`
  overlay.
- The size column shows the position value in USDT by default, and `n` swaps it
  to the venue's contract amount. Sorting by the column follows whichever unit is
  displayed, so the visible order always matches the visible numbers.
- Toggleable chart overlays: `m` shows or hides the moving averages, and `p`
  shows or hides the entry-price line of the position being charted. When the
  entry is outside the visible range the price range widens to include it, up to
  a bounded multiple of the candle range, so an extreme position cannot squash
  the candles into a line.

### Fixed

- Diagnostics no longer corrupt the UI. The interactive UI emitted `tracing`
  warnings to stderr while ratatui owned the screen, so a warning such as the
  silent-stream notice was painted over the header and stayed there, because
  ratatui only repaints cells that change. The UI now logs to
  `$XDG_STATE_HOME/cryptui/cryptui.log` (`$CRYPTUI_LOG` overrides it), and the
  headless commands keep logging to stderr.
- Chart overlays no longer recolour the candles they cross. A Braille cell holds
  one colour, and the moving averages and entry line were drawn after the
  candles, so wherever a line crossed a bar the bar took the line's colour —
  which reads as the bar changing direction. The candles are now drawn last, so
  a line is interrupted by the bars it crosses and the bars keep their colour.
- `--account <name>` now selects the opening account in the interactive UI. It
  was only honoured by the headless commands, so `cryptui --account paper`
  silently opened the configured default account instead. An unknown name is now
  reported as an error rather than falling back silently.

## [0.1.0] - 2026-09-30

The first release: read-only monitoring of Binance USDⓈ-M futures accounts.

### Added

- Configuration at `~/.config/cryptui/config.toml` (or `--config`, or
  `$CRYPTUI_CONFIG`), with `${ENV_VAR}` interpolation, several accounts, and a
  validation pass that rejects half-configured accounts with a clear message.
- Credentials held in a type whose `Debug` output is redacted, so a key cannot
  reach a log line or a panic message by accident. A file holding literal
  credentials is reported when its permissions are too open.
- Positions table: sortable by symbol, side, size, entry, mark, margin or PnL
  (largest winner first by default), with the active column marked, colour-coded
  PnL, and the selected contract preserved across refreshes and re-sorts.
- Candle chart: braille candles, MA(7/25/99) overlays, a volume pane, price and
  time axes, interval switching (1m to 1d), pan (`h`/`l`), zoom (`+`/`-`) and a
  follow mode that resumes at the newest candle.
- Symbol picker (`s`) over every tradable contract, with a filter that ranks
  name-prefix matches first. The chosen contract need not have an open position.
- Account switching (`a`) between every configured account, including offline
  fixture accounts that replay JSON instead of calling the network.
- Live updates: a WebSocket subscription per chart target and per held contract.
  Venues without streams, and streams that connect but stay silent, fall back to
  REST polling rather than showing a frozen chart.
- Feed health in the header and chart title: age, staleness and the reason a
  feed failed, instead of a quietly frozen screen.
- Headless entry points: `--print-config`, `--print symbols|positions|balances|
  klines`, and `--dump-frame` for a single ANSI frame, so every layer can be
  verified without a terminal.
- 145 tests, including layout snapshots rendered into a test backend, chart
  arithmetic, payload mapping, and the silent-stream fallback.

### Notes

- Read-only by construction: no order-placement code exists yet.
- Binance's USDⓈ-M futures market-stream host accepts WebSocket upgrades and
  then sends nothing from some networks, including the one this was developed
  on. All futures REST endpoints and the spot stream work, so the application
  polls instead. See the troubleshooting section of the README.

[Unreleased]: https://github.com/phl3x0r/cryptui/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/phl3x0r/cryptui/releases/tag/v0.1.0
