# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project uses
[semantic versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- Chart bars are all the same width, and evenly spaced, at every zoom level. A
  Braille cell holds two dots and one colour, and the candles were placed by
  fraction of a candle slot, so a bar came out one or two dots wide depending on
  where its slot fell and two neighbours could land in the same cell and erase
  each other — which read as bars merging and gaps flickering as the chart
  scrolled. Candles now sit on a pitch of whole cells: the same body width and
  the same gap for every bar. The pitch is the whole number of cells nearest the
  one that would have fitted the candles asked for, so the chart may show a few
  more or fewer of them than the viewport wanted — at a tight pitch the pane has
  no room for daylight and the bars touch, which is the one case no layout can
  avoid. Bodies are filled rather than outlined, which only showed at all once a
  body was several cells wide.
- Wicks sit on the middle of their bar. They were drawn one dot in from the bar's
  left edge whatever the bar's width, so a wide bar carried a wick near one side.
  A wick is now as wide as the grid allows it to be centred, and no wider than it
  must be to stay visible: a whole cell on the middle of an odd body of three
  cells or more, and a single dot otherwise — a dot grid cannot centre a wick on
  a one-cell bar, but a full-width wick there would be indistinguishable from the
  bar itself, which is worse than half a dot off centre.
- Every zoom press changes what is drawn. Candles are laid out on a pitch of
  whole cells, so a pane 195 columns wide draws 195 candles at one cell each and
  97 at two; a zoom step landing between the two drew exactly the same chart, so
  the key appeared to do nothing until it crossed the next boundary. A press now
  settles on the next pitch in its direction, and at the two ends of the zoom
  range — where the pane cannot show more, or the pitch cannot grow — it says so
  by leaving the chart alone.

## [0.1.3] - 2026-10-01

### Added

- An account panel beside the chart on screens wide enough to keep the whole
  positions table: margin ratio, maintenance margin, equity, unrealised PnL,
  position value, actual leverage, the account mode (multi-assets or
  single-asset) and every non-zero balance. Narrower screens drop it rather than
  squeeze the table, and the footer line carries the same summary at every width.

## [0.1.2] - 2026-09-30

### Added

- Account performance panel (`e`): a full-screen view of the equity curve and the
  figures that describe it — total return, CAGR, volatility, Sharpe, Sortino, max
  drawdown, Calmar, win rate, best and worst day — over a selectable window of one
  month, three months, a year or everything on record (`1`-`4`, `[`/`]`).
- The panel draws a performance index (100 at the start of the window, adjusted
  for deposits and withdrawals), marking each flow with `◆`; `b` swaps it for the
  raw wallet balance.
- `--print performance [--window 1m|3m|1y|all]` for the same figures without a
  terminal.
- The panel names the assets the income arrived in when they are not a dollar
  stablecoin, so a credits or multi-assets account is not read as a plain USDⓈ
  one.

### Fixed

- The performance curve fetched the *oldest* income records in the window rather
  than the newest. A request carrying a `startTime` is answered oldest-first, so
  an account with more than a page of records never had its recent days fetched,
  and walking back from today's balance credited that profit to the first day
  instead: a live account read `-0.3%` where the venue's own records say `+2.0%`.
  Each page is now bounded from above and the walk goes backwards, which keeps the
  recent end of the curve exact.

### Notes

- The curve is reconstructed from the venue's income records anchored on the
  current balance, net of deposits and withdrawals, and today's balance is
  recorded locally on each run. Binance serves only a few months of income and
  no equity history, so long windows show the coverage actually available, which
  the panel states explicitly.
- Income is not converted between assets: on a multi-assets or credits account
  (Credits Trading Mode settles PnL in `BNFCR`) the records are added at face
  value and the coverage line names them. That is deliberate rather than an
  oversight — the venue's own wallet total values a credit at about 0.884 USD
  (measured against the other assets it holds), while the venue's own PnL
  percentages divide the credit amount at par by a USDⓈ equity, so par is the
  convention that agrees with the page it will be compared against. Binance's PnL
  page also counts unrealised moves and history the income API will not serve, so
  the two are not the same quantity.

## [0.1.1] - 2026-09-30

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

[Unreleased]: https://github.com/phl3x0r/cryptui/compare/v0.1.3...HEAD
[0.1.3]: https://github.com/phl3x0r/cryptui/releases/tag/v0.1.3
[0.1.2]: https://github.com/phl3x0r/cryptui/releases/tag/v0.1.2
[0.1.1]: https://github.com/phl3x0r/cryptui/releases/tag/v0.1.1
[0.1.0]: https://github.com/phl3x0r/cryptui/releases/tag/v0.1.0
