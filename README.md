# cryptui

A terminal UI for keeping an eye on several crypto exchange accounts at once,
inspired by the dense, panel-based trading terminals of the desktop world.

**Status: early development (v0.1 in progress).** The first release is
deliberately **read-only** — it monitors, it does not trade yet.

Target of v0.1:

- multiple accounts from one config file, switchable at runtime
- sortable table of open positions with live unrealized PnL
- symbol picker (all USDTⓈ-M perpetuals) driving the chart
- candlestick chart with moving averages, volume subpane, interval switch, pan/zoom
- header with mark price, funding, 24h stats and feed freshness; footer with
  balance, equity, unrealized PnL and margin ratio

Planned afterwards, not implemented yet: order entry, order book, Binance Spot,
Bybit, and other venues (the venue layer is abstracted behind a trait so new
exchanges do not touch UI code).

## Install

Requires Rust 1.85 or newer.

```sh
cargo install --git https://github.com/phl3x0r/cryptui
```

## Configure

CryptUI reads its configuration from the first of:

1. `--config <path>`
2. `$CRYPTUI_CONFIG`
3. `~/.config/cryptui/config.toml`

Start from the template in this repository:

```sh
mkdir -p ~/.config/cryptui
cp config.template.toml ~/.config/cryptui/config.toml
chmod 600 ~/.config/cryptui/config.toml
```

Then fill in your credentials. Read-only API keys are sufficient and
recommended: enable *Reading* only, plus futures read access. CryptUI never
needs trade permission in v0.1. Secret values may be written literally or as
`${ENV_VAR}` references, which are resolved from the environment at load time —
prefer the environment so the file never holds secret material.

The template also defines a fixture account that serves recorded payloads
instead of calling the network, so multi-account switching can be exercised
offline.

## Development

Install the pinned toolchain and run the same checks as CI:

```sh
mise install          # Rust toolchain pinned by .mise.toml
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```

## License

MIT — see [LICENSE](LICENSE).
