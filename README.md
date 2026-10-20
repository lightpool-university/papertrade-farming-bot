# papertrade-farming-bot

Hedge bot for PAPER. It opens an equal long and short, closes the pair on a target move or near liquidation, then stakes PAPER and claims rewards.

Each leg uses half of the current available balance. Leverage defaults to 50.

Mark price always comes from Hyperliquid BTC last trade. If no trade arrives for 1 second, the bot polls `allMids`. Both venues share this price feed.

The terminal UI keeps `AccountStore` on top (balances, positions, round cost / farm) and scrolls logs below. Quit with `q` or Ctrl-C.

## Venues

| Venue | Env value | Alias | What it talks to |
|---|---|---|---|
| Papertrade on Hyperliquid | `papertrade_on_hyperliquid` (default) | `exchange` | Live Papertrade on HyperEVM (`exchange.papertrade.xyz`) |
| Papertrade on LightPool | `papertrade_on_lightpool` | `lightpool` | Local / LightPool indexer only (no papertrade-app) |

Do not open the Papertrade web UI with the same wallet while the bot is running. The page registers its own session / agent and can replace the bot’s key, so intents or txs will fail.

## Build

```bash
cargo build --release
```

The binary is `target/release/papertrade-farming-bot`.

## Papertrade on Hyperliquid

Spends real USDC on HyperEVM.

```bash
export PAPERTRADE_WALLET_KEY=0x...
# optional; this is the default
export PAPERTRADE_VENUE=papertrade_on_hyperliquid
cargo run --release
```

- HTTP base: `PAPERTRADE_HTTP_URL` or `https://exchange.papertrade.xyz`
- Intents: `POST /intents`
- Account stream: SSE `GET /state/user/live`
- Auth: wallet registers a session key (`data/session.key`, mode `0600`) and `data/nonces.json`

If Hyperliquid is unreachable, the bot asks for a proxy (`HL_PROXY` / stdin) and can save it under `data/hl_proxy.env`.

Open limits, from `GET /state/trading`:

| Limit | Value |
|---|---|
| Minimum margin | $10 |
| Minimum notional (margin × leverage) | $1,000 |
| Maximum notional | $10,000,000 |
| Maximum leverage | 1000 |

A balance that is too small for those limits is rejected by the exchange.

## Papertrade on LightPool

Talks only to the LightPool indexer. Does not use papertrade-app HTTP for trading.

```bash
export PAPERTRADE_VENUE=papertrade_on_lightpool
# optional; Anvil #2 is used when unset
# export PAPERTRADE_WALLET_KEY=0x...
cargo run --release
```

- Submit: `PAPERTRADE_HTTP_URL` → `POST /api/tx/submit` (default `http://127.0.0.1:3002`)
- Account state: `PAPERTRADE_WS_URL` → `paper.user.state` (default `ws://127.0.0.1:3002/api/ws`)
- Market: `PAPER_MARKET` (default `0x0c01000000000001`), instrument `PAPER_INSTRUMENT` (default `0` = BTC)
- Auth: `set_agent` with `data/agent.key`; after success writes `data/agent.auth`

Default wallet when `PAPERTRADE_WALLET_KEY` is unset (Anvil account #2):

- address `0x3C44CdDdB6a900fa2b585dd299e03d12FA4293BC`
- key `0x5de4111afa1a4b94908f83103eb1f1706367c2e68ca870fc3fb9a804cdab365a`

## Settings

Flags and environment variables use the same names. `--leverage 50` is `PAPERTRADE_LEVERAGE=50`.

| Env | Default | Meaning |
|---|---|---|
| `PAPERTRADE_WALLET_KEY` | required on Hyperliquid; Anvil #2 on LightPool | Wallet private key. `0x` is optional. |
| `PAPERTRADE_VENUE` | `papertrade_on_hyperliquid` | `papertrade_on_hyperliquid` / `exchange`, or `papertrade_on_lightpool` / `lightpool`. |
| `PAPERTRADE_HTTP_URL` | Hyperliquid: `https://exchange.papertrade.xyz`; LightPool: `http://127.0.0.1:3002` | Venue HTTP base. |
| `PAPERTRADE_WS_URL` | `ws://127.0.0.1:3002/api/ws` | LightPool indexer WebSocket (`paper.user.state`). |
| `PAPERTRADE_DATA_DIR` | `data` | Session key, agent key, nonces, proxy file. |
| `PAPERTRADE_COIN` | `BTC` | Hyperliquid price coin. |
| `PAPERTRADE_LEVERAGE` | `50` | Leverage for both legs. |
| `PAPERTRADE_TARGET_MOVE_BPS` | `60` | Close the pair after this adverse move (basis points). |
| `PAPERTRADE_LIQ_BUFFER_BPS` | `40` | Close when mark is this close to bust (basis points). |
| `PAPERTRADE_PRICE_STALE_MS` | `1000` | Poll `allMids` when the trade feed is quiet. |
| `PAPERTRADE_MIN_STAKE` | `1000` | Stake when PAPER balance reaches this many tokens. |
| `PAPER_MARKET` | `0x0c01000000000001` | Paper market contract (LightPool). |
| `PAPER_INSTRUMENT` | `0` | Instrument id. `0` is BTC. |
| `HYPERLIQUID_WS_URL` | `wss://api.hyperliquid.xyz/ws` | Trade price WebSocket. |
| `HYPERLIQUID_INFO_URL` | `https://api.hyperliquid.xyz/info` | `allMids` fallback. |
| `RUST_LOG` | `info` | Log filter. |

`data/` is gitignored. Do not commit `session.key`, `agent.key`, `agent.auth`, or `nonces.json`.
