use anyhow::Result;
use rand::RngExt;
use rust_decimal::{Decimal, dec, prelude::*};
use std::{collections::HashMap, time::Duration};

use crate::{
    client::{Client, ClientError},
    config::Config,
    types::{AssetResponse, OrderResponse, OrderSide, OrderType},
};

pub struct Bot {
    client: Client,
    config: Config,
    assets: HashMap<String, AssetResponse>,
}

#[derive(Debug)]
struct PairBalance {
    pub base_balance: Decimal,
    pub quote_balance: Decimal,
}

/*
- Bot needs to login
- Get all asset
- loop at interval
- Get all trading pair
- Select a random pair
- Get Ticker price for that pair
- Check current open orders
    if deviated from stale threshold
        close
    if not
        continue
- check balance of quote and base for selected pair
    if lower than minimum balance
        top up to target balance
- Decide to be a taker or maker
    if Taker
        - Decide whether to buy or sell
            if buy
                compute how much base asset to buy
                compare price of base computed relative to current quote balance
                    if higher
                        scale down to an amount the current Usdt balance can cover
                    if lower
                        proceed to buy
            if Sell
                compute how much base asset to sell
                check if base balance can cover base amount to sell
                    if higher
                        scale down
                    if lower
                        proceed to sell
    if Maker
        check number of current open orders
            if >= ORDER_CAP
                return;
        for Ask
            compute ask price * (1 - spread)
            compute random quantity
                if > base balance
                    scale down
                if < base balance
                    place limit order
        for Bids
            compute bid price * (1 + spread)
            compute random quantity
                if (qty * bid price) > quote balance
                    scale down
                if < quote balance
                    place limit order
*/

/*
Known limitations
- access token expires every 15 mins: the bot can encounter a 401 error, it needs to refresh the token, if that doesn't work it needs to re-login. then still recall the function where it faced the error and proceed with the remaining steps.
- the endpoints are rate limited
*/

macro_rules! try_call {
    ($self:ident, $call:expr) => {
        match $call.await {
            Err(ClientError::Unauthorized) => {
                $self.reauthenticate().await?;
                $call.await
            }
            other => other,
        }
    };
}

impl Bot {
    pub fn new(config: Config) -> Self {
        let base_url = format!("{}/api/v1", &config.exchange_url);
        Bot {
            client: Client::new(&base_url),
            config,
            assets: HashMap::new(),
        }
    }

    pub async fn run(&mut self) {
        if let Err(e) = self
            .client
            .login(&self.config.email, &self.config.password)
            .await
        {
            tracing::error!(error = %e, "Failed to login, exiting");
            return;
        }

        tracing::info!(email = %self.config.email, "Bot logged in successfully");

        match self.client.get_assets().await {
            Ok(assets) => {
                self.assets = assets.into_iter().map(|a| (a.symbol.clone(), a)).collect();
                tracing::info!(count = %self.assets.len(), "Assets loaded");
            }
            Err(e) => {
                tracing::error!(error = %e, "Failed to fetch assets, exiting");
                return;
            }
        }

        loop {
            if let Err(e) = self.cycle().await {
                tracing::error!(error = %e, "Cycle failed, skipping");
            }
            tokio::time::sleep(Duration::from_secs(self.config.interval_secs)).await;
        }
    }

    async fn cycle(&mut self) -> Result<()> {
        let pairs = match try_call!(self, self.client.get_active_pairs()) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(error = %e, "Failed to fetch active pairs");
                return Ok(());
            }
        };

        if pairs.is_empty() {
            tracing::warn!("No active pairs available, skipping cycle");
            return Ok(());
        }

        let selected_pair = pairs[random_number(0_usize, pairs.len() - 1_usize)].clone();
        let symbol = selected_pair.symbol.as_str();
        let base_asset = selected_pair.base_asset.as_str();
        let quote_asset = selected_pair.quote_asset.as_str();

        tracing::info!(symbol = %symbol, "Selected pair for this cycle");

        let mid_price = match self.get_mid_price(symbol, base_asset).await {
            Some(price) => price,
            None => {
                tracing::warn!(symbol = %symbol, "No price available, skipping cycle");
                return Ok(());
            }
        };

        let open_orders = match try_call!(self, self.client.get_open_orders(&symbol)) {
            Ok(orders) => orders,
            Err(e) => {
                tracing::warn!(symbol = %symbol, error = %e, "Failed to fetch open orders, skipping cycle");
                return Ok(());
            }
        };

        // Cancel stale orders, keep fresh ones
        let mut remaining_orders = Vec::new();
        for order in open_orders {
            let order_price = match order.price {
                Some(price) => price,
                None => {
                    tracing::warn!(order_id = %order.id, "Limit order missing price, skipping");
                    continue;
                }
            };

            let drift = (mid_price - order_price).abs() / mid_price;
            if drift > self.config.stale_threshold {
                if let Err(e) = self.client.cancel_order(order.id).await {
                    tracing::warn!(error = %e, order_id = %order.id, "Failed to cancel stale order");
                }
            } else {
                remaining_orders.push(order);
            }
        }

        let pair_balance = match self.ensure_balance(base_asset, quote_asset).await {
            Ok(Some(pair_balance)) => pair_balance,
            _ => return Ok(()),
        };

        eprintln!("pair balance: {:?}", pair_balance);

        // Roll taker dice
        if random_decimal(dec!(0.0), dec!(1.0))
            < Decimal::from_f64(self.config.taker_prob).unwrap_or(dec!(0.15))
        {
            eprintln!("Inside taker..............................");
            self.taker_cycle(&symbol, &pair_balance).await?;
        } else {
            eprintln!("Inside maker..............................");
            self.maker_cycle(&symbol, mid_price, &pair_balance, remaining_orders)
                .await?;
        }

        Ok(())
    }

    async fn maker_cycle(
        &mut self,
        symbol: &str,
        mid_price: Decimal,
        available_balance: &PairBalance,
        open_orders: Vec<OrderResponse>,
    ) -> Result<()> {
        let bids_count = open_orders
            .iter()
            .filter(|o| o.side == OrderSide::Buy)
            .count();
        let asks_count = open_orders.len() - bids_count;

        if bids_count < self.config.order_cap as usize {
            let bid_price = mid_price * (dec!(1) - self.config.spread);
            let quantity = random_base_quantity_to_buy(available_balance.quote_balance, bid_price);

            if let Err(e) = self
                .client
                .place_order(
                    symbol,
                    OrderSide::Buy,
                    OrderType::Limit,
                    Some(bid_price),
                    quantity,
                )
                .await
            {
                tracing::warn!(error = %e, "Failed to place maker bid");
            } else {
                tracing::info!(symbol = %symbol, price = %bid_price, quantity = %quantity, "Placed maker bid");
            }
        }

        if asks_count < self.config.order_cap as usize {
            let ask_price = mid_price * (dec!(1) + self.config.spread);
            let quantity = random_base_quantity_to_sell(available_balance.base_balance);

            if let Err(e) = self
                .client
                .place_order(
                    symbol,
                    OrderSide::Sell,
                    OrderType::Limit,
                    Some(ask_price),
                    quantity,
                )
                .await
            {
                tracing::warn!(error = %e, "Failed to place maker ask");
            } else {
                tracing::info!(symbol = %symbol, price = %ask_price, quantity = %quantity, "Placed maker ask");
            }
        }

        Ok(())
    }

    async fn taker_cycle(&mut self, symbol: &str, available_balance: &PairBalance) -> Result<()> {
        let orderbook = match try_call!(self, self.client.get_orderbook(symbol)) {
            Ok(o) => o,
            Err(e) => {
                tracing::warn!(symbol = %symbol, error = %e, "Failed to get orderbook");
                return Ok(());
            }
        };

        // Coin flip
        if random_number(0, 1) == 0 {
            let best_ask = match orderbook.asks.first() {
                Some(level) => level.price,
                None => {
                    tracing::warn!(symbol = %symbol, "Ask side empty, skipping taker buy");
                    return Ok(());
                }
            };

            // let price = best_ask * (dec!(1) + dec!(0.001));
            let price = best_ask;
            let quantity = random_base_quantity_to_buy(available_balance.quote_balance, price);

            if let Err(e) = self
                .client
                .place_order(
                    symbol,
                    OrderSide::Buy,
                    OrderType::Market,
                    None,
                    quantity,
                )
                .await
            {
                tracing::warn!(error = %e, "Failed to place taker buy");
            } else {
                tracing::info!(symbol = %symbol, price = %price, quantity = %quantity, "Placed taker buy");
            }
        } else {
            let best_bid = match orderbook.bids.first() {
                Some(level) => level.price,
                None => {
                    tracing::warn!(symbol = %symbol, "Bid side empty, skipping taker sell");
                    return Ok(());
                }
            };

            // let price = best_bid * (dec!(1) - dec!(0.001));
            let price = best_bid;
            let quantity = random_base_quantity_to_sell(available_balance.base_balance);

            if let Err(e) = self
                .client
                .place_order(
                    symbol,
                    OrderSide::Sell,
                    OrderType::Market,
                    None,
                    quantity,
                )
                .await
            {
                tracing::warn!(error = %e, "Failed to place taker sell");
            } else {
                tracing::info!(symbol = %symbol, price = %price, quantity = %quantity, "Placed taker sell");
            }
        }

        Ok(())
    }

    async fn ensure_balance(
        &mut self,
        base_asset: &str,
        quote_asset: &str,
    ) -> Result<Option<PairBalance>> {
        let balances = match try_call!(self, self.client.get_balances()) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(error = %e, "Failed to fetch balances, skipping cycle");
                return Ok(None);
            }
        };

        let base_balance = balances
            .iter()
            .find(|b| b.asset == base_asset)
            .map(|b| b.available)
            .unwrap_or(Decimal::ZERO);

        let quote_balance = balances
            .iter()
            .find(|b| b.asset == quote_asset)
            .map(|b| b.available)
            .unwrap_or(Decimal::ZERO);

        tracing::info!(
            base_asset = %base_asset,
            quote_asset = %quote_asset,
            base_balance = %base_balance,
            quote_balance = %quote_balance,
            "Balance check"
        );

        let mut balances = vec![];

        for (asset, balance) in [(base_asset, base_balance), (quote_asset, quote_balance)] {
            match self.deposit_asset(asset, balance).await {
                Some(balance) => balances.push(balance),
                None => return Ok(None),
            }
        }

        Ok(Some(PairBalance {
            base_balance: balances[0],
            quote_balance: balances[1],
        }))
    }

    async fn deposit_asset(&mut self, asset: &str, asset_balance: Decimal) -> Option<Decimal> {
        if asset_balance < get_min_balance(asset) {
            let target = max_deposit(asset);
            if target > Decimal::ZERO {
                match self.client.deposit(asset, target).await {
                    Ok(_) => {
                        tracing::info!(asset = %asset, amount = %target, "Deposited asset");
                        return Some(target + asset_balance);
                    }
                    Err(ClientError::RateLimited) => {
                        tracing::warn!(asset = %asset, "Deposit rate limited, skipping cycle");
                        return None;
                    }
                    Err(e) => {
                        tracing::warn!(asset = %asset, error = %e, "Failed to deposit quote, skipping cycle");
                        return None;
                    }
                }
            } else {
                tracing::warn!(asset = %asset, "No target balance for");
                return None;
            }
        }

        Some(asset_balance)
    }

    async fn get_mid_price(&self, symbol: &str, base_asset: &str) -> Option<Decimal> {
        match self.client.get_ticker(symbol).await {
            Ok(ticker) => return Some(ticker.last_price),
            Err(ClientError::Other(_)) => {
                tracing::warn!(symbol = %symbol, "Ticker not available, trying CoinGecko fallback");
            }
            Err(e) => {
                tracing::warn!(symbol = %symbol, error = %e, "Ticker failed");
                return None;
            }
        }

        // try CoinGecko using coingecko_id from loaded assets
        let coingecko_id = self
            .assets
            .get(base_asset)
            .and_then(|a| a.coingecko_id.as_deref().map(|s| s.to_string()));

        if let Some(id) = coingecko_id {
            match self.fetch_coingecko_price(&id).await {
                Ok(price) => {
                    tracing::info!(asset = %base_asset, price = %price, "Price from CoinGecko");
                    return Some(price);
                }
                Err(e) => {
                    tracing::warn!(asset = %base_asset, error = %e, "CoinGecko fallback failed")
                }
            }
        }

        // hardcoded fallback
        let price = match base_asset {
            "BTC" => dec!(70000),
            "ETH" => dec!(2500),
            "SOL" => dec!(80),
            _ => {
                tracing::warn!(asset = %base_asset, "No fallback price available");
                return None;
            }
        };

        tracing::warn!(asset = %base_asset, price = %price, "Using hardcoded fallback price");
        Some(price)
    }

    async fn fetch_coingecko_price(&self, coingecko_id: &str) -> Result<Decimal> {
        let url = format!(
            "https://api.coingecko.com/api/v3/simple/price?ids={}&vs_currencies=usd",
            coingecko_id
        );

        let res: HashMap<String, serde_json::Value> = self.client.http_get_external(&url).await?;

        let price = res
            .get(coingecko_id)
            .and_then(|v| v.get("usd"))
            .and_then(|v| v.as_f64())
            .ok_or_else(|| anyhow::anyhow!("No price in CoinGecko response"))?;

        Decimal::from_f64(price)
            .ok_or_else(|| anyhow::anyhow!("Failed to convert CoinGecko price to Decimal"))
    }

    async fn reauthenticate(&mut self) -> Result<(), ClientError> {
        tracing::info!("Access token expired, attempting refresh");
        if self.client.refresh().await.is_err() {
            tracing::info!("Refresh failed, re-logging in");
            self.client
                .login(&self.config.email, &self.config.password)
                .await?;
        }
        tracing::info!("Re-authentication successfully");
        Ok(())
    }
}

pub fn random_number(min: usize, max: usize) -> usize {
    rand::rng().random_range(min..=max)
}

pub fn random_decimal(min: Decimal, max: Decimal) -> Decimal {
    let min_f = min.to_f64().unwrap_or(0.0);
    let max_f = max.to_f64().unwrap_or(1.0);
    let val = rand::rng().random_range(min_f..=max_f);
    Decimal::from_f64(val).unwrap_or(min)
}

// Deposit targets per asset — set just below the exchange's per-request deposit cap.
// Hardcoded temporarily. When new assets are added to the exchange, add them here.
// See: BalanceRequest::validate_deposit in the exchange codebase.
fn max_deposit(asset: &str) -> Decimal {
    match asset {
        "USDT" => dec!(1000),
        "BTC" => dec!(0.05),
        "ETH" => dec!(0.5),
        "SOL" => dec!(5),
        _ => {
            tracing::warn!(asset = %asset, "Unknown asset, no deposit target defined");
            dec!(0)
        }
    }
}

/*
This compute a valid base quantity that can be bought
- balance: balance of quote asset
- price: price for a single unit of base asset
*/
fn random_base_quantity_to_buy(quote_balance: Decimal, price: Decimal) -> Decimal {
    let random_num = random_decimal(dec!(0.1), dec!(1));

    // compute random balance amount to spend
    let amount = random_num * quote_balance;

    // valid quantity that can be bought
    amount / price
}

fn random_base_quantity_to_sell(base_balance: Decimal) -> Decimal {
    let random_num = random_decimal(dec!(0.1), dec!(1));

    base_balance * random_num
}


fn get_min_balance(asset: &str) -> Decimal {
    max_deposit(asset) / dec!(10)
}
