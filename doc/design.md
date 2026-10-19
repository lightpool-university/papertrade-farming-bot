# Papertrade Farming

Hedge-farm PAPER on Papertrade BTC (`https://exchange.papertrade.xyz/trade/btc`, HyperEVM), then stake PAPER and claim USDC.

Mark price comes from the Hyperliquid `trades` feed. Position status comes from the Papertrade account SSE (`GET /state/user/live`). Opens, closes, and cancels are session-key intents. Stake and claim are wallet-signed intents. Both are posted to `POST /intents`.

## Tasks

1. **Sign and submit.** The wallet registers a session key. The session key signs open, close, and cancel. The wallet signs stake and claim. Post each intent to `/intents`. Persist nonces.
2. **Account stream.** Subscribe to the Papertrade account SSE. Apply snapshots and patches. Track positions, pending intents, PAPER balance, and claimable rewards. Reconnect when the revision does not match.
3. **Price.** Read the BTC last trade from the Hyperliquid `trades` feed. If no trade arrives for 1 second, poll `allMids`. The hedge loop runs on each price.
4. **Hedge loop.** When flat, open an equal long and short. If only one leg fills, cancel the other and close the filled leg. When both legs are open, watch the losing leg and close both at the target move or near liquidation.
5. **Stake loop.** After both legs are closed, stake PAPER. Claim when rewards are pending. Then return to task 4.
