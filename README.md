# Excentra Pulse

**Market simulation bot for Excentra Exchange**

Excentra Pulse keeps the order book alive by simulating real trading activity. It runs multiple role-specialized bots as concurrent tokio tasks — makers provide liquidity, takers consume it — generating realistic market depth and price discovery without real users.

---

## Why It Exists

Excentra Exchange is a fully functional centralized exchange, but an exchange with no users has an empty order book and no trades. Pulse solves this by acting as the first participant — seeding the book with limit orders at realistic prices and continuously matching against them with market orders, keeping the 24h chart populated and the exchange looking alive.

---

## How It Works

### Bot Roles

**Maker bots** maintain order book depth. Each cycle they:
- Fetch open orders and cancel any that have drifted too far from the current mid price (stale threshold)
- Place a new bid at `mid * (1 - spread)` and ask at `mid * (1 + spread)`
- Respect a per-bot order cap to avoid flooding the book

**Taker bots** consume liquidity. Each cycle they:
- Fetch the current orderbook
- Pick a direction (buy or sell) influenced by a `Bias` — Bullish, Bearish, or Neutral
- Place a market order sized proportionally to available balance and current bias
- Rotate bias every N cycles

### Bias System

The taker's `Bias` controls the probability of buying vs selling:

| Bias | Buy probability | Sell probability |
|---|---|---|
| Bullish | 70% | 30% |
| Neutral | 50% | 50% |
| Bearish | 30% | 70% |

Bias is randomly selected and held for a configurable number of cycles before rotating, simulating realistic directional momentum.

### Architecture

All bots run as isolated `tokio` tasks within a single process. Each bot has its own `reqwest::Client` with an independent cookie jar — critical for correct session isolation, since the exchange uses httpOnly cookie-based refresh tokens.

```
main
├── PriceService task (CoinGecko, refreshes every 24h)
├── Bot 1 task (maker)
├── Bot 2 task (taker)
└── Bot 3 task (maker)
```

### Price Resolution

Mid price is resolved in order of preference:
1. Exchange ticker (last traded price)
2. CoinGecko cached price (refreshed daily)
3. Hardcoded fallback (BTC: 75000, ETH: 2500, SOL: 85)

### Resilience

- **Token expiry**: detects 401s, attempts refresh, falls back to re-login, then retries the failed call via a `try_call!` macro (chosen over async closures to avoid borrow checker conflicts with `&mut self`)
- **Rate limiting**: parses `retry-after` from exchange responses and backs off for the specified duration
- **Startup cleanup**: taker bots cancel all resting orders on startup to clear state from previous runs

---

## Getting Started

**Prerequisites:** [Rust](https://rustup.rs/) (stable), a running [Excentra](https://github.com/manlikeHB/excentra) instance, and bot accounts registered on the exchange.

**1. Clone and configure**

```bash
git clone https://github.com/manlikeHB/excentra_pulse.git
cd excentra_pulse
cp .env.example .env
```

Edit `.env` with your exchange URL and bot credentials:

```env
EXCHANGE_URL=http://localhost:5098
BOT_COUNT=3

BOT_1_EMAIL=bot1@excentra.com
BOT_1_PASSWORD=your-password
BOT_1_ROLE=maker
BOT_1_SPREAD=0.002
BOT_1_INTERVAL_SECS=10
BOT_1_STALE_THRESHOLD=0.005
BOT_1_ORDER_CAP=3

BOT_2_EMAIL=bot2@excentra.com
BOT_2_PASSWORD=your-password
BOT_2_ROLE=taker
BOT_2_INTERVAL_SECS=15

BOT_3_EMAIL=bot3@excentra.com
BOT_3_PASSWORD=your-password
BOT_3_ROLE=maker
BOT_3_SPREAD=0.008
BOT_3_INTERVAL_SECS=20
BOT_3_STALE_THRESHOLD=0.015
BOT_3_ORDER_CAP=5
```

**2. Run**

```bash
cargo run
```

---

## Docker

**Prerequisites:** [Docker](https://www.docker.com/), Excentra running on the same Docker network.

```bash
cp .env.example .env  # fill in values
docker compose up -d
```

Pulse connects to the exchange via the `excentra-net` Docker network. Set `EXCHANGE_URL=http://api:5098` when running both stacks together.

If the network doesn't exist yet:

```bash
docker network create excentra-net
```

---

## Configuration Reference

| Variable | Role | Description |
|---|---|---|
| `EXCHANGE_URL` | all | Base URL of the Excentra API (no `/api/v1` suffix) |
| `BOT_COUNT` | all | Number of bots to spawn |
| `BOT_N_EMAIL` | all | Bot account email |
| `BOT_N_PASSWORD` | all | Bot account password |
| `BOT_N_ROLE` | all | `maker` or `taker` |
| `BOT_N_INTERVAL_SECS` | all | Cycle interval in seconds |
| `BOT_N_SPREAD` | maker | Spread from mid price (e.g. `0.002` = 0.2%) |
| `BOT_N_STALE_THRESHOLD` | maker | Max price drift before cancelling an order (e.g. `0.005` = 0.5%) |
| `BOT_N_ORDER_CAP` | maker | Max open orders per side per pair |

---

## Testing

Unit tests cover bot logic using a `MockClient` that implements `ExchangeClient` — no exchange connection required:

```bash
cargo test
```

Key test cases: maker order placement and cap enforcement, stale order cancellation, taker market-only invariant (1000 iterations — regression guard against the taker ever placing limit orders), bias-driven buy/sell paths, backoff behaviour, deposit triggers, and price resolution fallback.

---

## Known Limitations

**Deposit rate limiting** — each bot account has a 60/hour deposit limit on the exchange. With 3 pairs × 2 assets per cycle and fast cycling, bots can exhaust this limit within an hour. When hit, bots back off for the duration specified in the exchange response and resume automatically.

**Single exchange** — Pulse is designed for Excentra only. The `ExchangeClient` trait could be implemented for other exchanges, but price resolution and deposit logic are Excentra-specific.

**Simulated funds** — all deposits are simulated. Pulse is not designed for use with real funds or production exchanges.