# papertrade-farming-bot

Hedge bot for PAPER. It opens an equal long and short, closes the pair on a target move or near liquidation, then stakes PAPER and claims rewards.

Each leg uses half of the current available balance. Leverage defaults to 50. The price feed is the Hyperliquid BTC last trade. If no trade arrives for 1 second, the bot polls `allMids`.

The online venue spends real USDC on HyperEVM.

## Build

```bash
cargo build --release
```

The binary is `target/release/papertrade-farming-bot`.

## Run on online Papertrade

```bash
export PAPERTRADE_WALLET_KEY=0x...
cargo run --release
```

Default venue is `papertrade_on_hyperliquid`. The HTTP base is `https://exchange.papertrade.xyz`. The same URL is used for `POST /intents` and the account SSE `GET /state/user/live`.

The first start writes `data/session.key` (mode `0600`) and `data/nonces.json`. The wallet registers that session key. Later starts reuse the same files.

The terminal UI keeps `AccountStore` fixed on top and scrolls logs below. Quit with `q` or Ctrl-C.

Online open limits, from `GET /state/trading`:

| Limit | Value |
|---|---|
| Minimum margin | $10 |
| Minimum notional (margin × leverage) | $1,000 |
| Maximum notional | $10,000,000 |
| Maximum leverage | 1000 |

A balance that is too small for those limits is rejected by the exchange.

## Run on LightPool

The bot talks only to the indexer. Submits go to `PAPERTRADE_HTTP_URL` (`POST /api/tx/submit`; default `http://127.0.0.1:3002` on LightPool). Account state comes from `PAPERTRADE_WS_URL` (default `ws://127.0.0.1:3002/api/ws`). Default `PAPER_MARKET` is `0x0c01000000000001`.

```bash
export PAPERTRADE_VENUE=papertrade_on_lightpool
cargo run --release
```

`PAPERTRADE_WALLET_KEY` is optional on LightPool. When unset, the bot uses Anvil account #2:

- address `0x3C44CdDdB6a900fa2b585dd299e03d12FA4293BC`
- key `0x5de4111afa1a4b94908f83103eb1f1706367c2e68ca870fc3fb9a804cdab365a`

`papertrade_on_lightpool` and `lightpool` are the same venue. The first start writes `data/agent.key` and, after `set_agent` succeeds, `data/agent.auth`.

## Settings

Flags and environment variables are the same names. `--leverage 50` is `PAPERTRADE_LEVERAGE=50`.

| Env | Default | Meaning |
|---|---|---|
| `PAPERTRADE_WALLET_KEY` | required online; Anvil #2 on LightPool | Wallet private key. `0x` is optional. |
| `PAPERTRADE_VENUE` | `papertrade_on_hyperliquid` | `papertrade_on_hyperliquid` or `papertrade_on_lightpool`. Aliases: `exchange`, `lightpool`. |
| `PAPERTRADE_HTTP_URL` | `https://exchange.papertrade.xyz` online; `http://127.0.0.1:3002` on LightPool | Venue HTTP base. Online: intents and account SSE. LightPool: indexer `POST /api/tx/submit`. |
| `PAPERTRADE_WS_URL` | `ws://127.0.0.1:3002/api/ws` | Indexer WebSocket for LightPool account state. |
| `PAPERTRADE_DATA_DIR` | `data` | Session key, agent key, and nonce files. |
| `PAPERTRADE_COIN` | `BTC` | Hyperliquid price coin. |
| `PAPERTRADE_LEVERAGE` | `50` | Leverage for both legs. |
| `PAPERTRADE_TARGET_MOVE_BPS` | `60` | Close the pair after this adverse move, in basis points. |
| `PAPERTRADE_LIQ_BUFFER_BPS` | `40` | Close the pair when price is this close to the bust price, in basis points. |
| `PAPERTRADE_PRICE_STALE_MS` | `1000` | Poll `allMids` when the trade feed is quiet for this long. |
| `PAPERTRADE_MIN_STAKE` | `1000` | Stake when the PAPER balance reaches this many tokens. |
| `PAPER_MARKET` | `0x0c01000000000001` | Paper market contract. |
| `PAPER_INSTRUMENT` | `0` | Instrument id. `0` is BTC. |
| `HYPERLIQUID_WS_URL` | `wss://api.hyperliquid.xyz/ws` | Trade price WebSocket. |
| `HYPERLIQUID_INFO_URL` | `https://api.hyperliquid.xyz/info` | `allMids` fallback. |
| `RUST_LOG` | `info` | Log filter. |

`data/` is gitignored. Do not commit `session.key`, `agent.key`, or `nonces.json`.
