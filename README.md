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

Stop with Ctrl-C.

Online open limits, from `GET /state/trading`:

| Limit | Value |
|---|---|
| Minimum margin | $10 |
| Minimum notional (margin × leverage) | $1,000 |
| Maximum notional | $10,000,000 |
| Maximum leverage | 1000 |

A balance that is too small for those limits is rejected by the exchange.

## Run on LightPool

The bot talks only to the indexer. `PAPERTRADE_WS_URL` is the indexer WebSocket. Submits go to `POST /tx/submit` on the same host (`ws://127.0.0.1:3002/ws` becomes `http://127.0.0.1:3002/tx/submit`). `PAPER_MARKET` is required.

```bash
export PAPERTRADE_WALLET_KEY=0x...
export PAPERTRADE_VENUE=papertrade_on_lightpool
export PAPERTRADE_WS_URL=ws://127.0.0.1:3002/ws
export PAPER_MARKET=0x...
cargo run --release
```

`papertrade_on_lightpool` and `lightpool` are the same venue. The first start writes `data/agent.key` and, after `set_agent` succeeds, `data/agent.auth`.

## Settings

Flags and environment variables are the same names. `--leverage 50` is `PAPERTRADE_LEVERAGE=50`.

| Env | Default | Meaning |
|---|---|---|
| `PAPERTRADE_WALLET_KEY` | required | Wallet private key. `0x` is optional. |
| `PAPERTRADE_VENUE` | `papertrade_on_hyperliquid` | `papertrade_on_hyperliquid` or `papertrade_on_lightpool`. Aliases: `exchange`, `lightpool`. |
| `PAPERTRADE_HTTP_URL` | `https://exchange.papertrade.xyz` | Online Papertrade base URL. LightPool does not use this. |
| `PAPERTRADE_WS_URL` | `ws://127.0.0.1:3002/ws` | Indexer WebSocket. LightPool also posts transactions to this host. |
| `PAPERTRADE_DATA_DIR` | `data` | Session key, agent key, and nonce files. |
| `PAPERTRADE_COIN` | `BTC` | Hyperliquid price coin. |
| `PAPERTRADE_LEVERAGE` | `50` | Leverage for both legs. |
| `PAPERTRADE_TARGET_MOVE_BPS` | `60` | Close the pair after this adverse move, in basis points. |
| `PAPERTRADE_LIQ_BUFFER_BPS` | `40` | Close the pair when price is this close to the bust price, in basis points. |
| `PAPERTRADE_PRICE_STALE_MS` | `1000` | Poll `allMids` when the trade feed is quiet for this long. |
| `PAPERTRADE_MIN_STAKE` | `1000` | Stake when the PAPER balance reaches this many tokens. |
| `PAPER_MARKET` | empty | Paper market contract. Required on LightPool. |
| `PAPER_INSTRUMENT` | `0` | Instrument id. `0` is BTC. |
| `HYPERLIQUID_WS_URL` | `wss://api.hyperliquid.xyz/ws` | Trade price WebSocket. |
| `HYPERLIQUID_INFO_URL` | `https://api.hyperliquid.xyz/info` | `allMids` fallback. |
| `RUST_LOG` | `info` | Log filter. |

`data/` is gitignored. Do not commit `session.key`, `agent.key`, or `nonces.json`.
