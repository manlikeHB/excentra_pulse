use crate::{
    constants,
    utils::{
        balance::{get_min_balance, max_deposit},
        maths::{
            random_base_quantity_to_buy, random_base_quantity_to_sell, random_decimal,
            random_number, random_quote_to_spend,
        },
    },
};

use anyhow::Result;
use rust_decimal::{Decimal, dec};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use crate::{
    client::{ClientError, ExchangeClient},
    config::{BotRole, Config},
    price_service::PriceService,
    types::{OrderResponse, OrderSide, OrderType},
};

pub struct Bot<T: ExchangeClient> {
    client: T,
    config: Config,
    taker_state: Option<TakerState>,
    price_service: Arc<PriceService>,
    backoff_until: Option<Instant>,
}

#[derive(Debug)]
struct PairBalance {
    pub base_balance: Decimal,
    pub quote_balance: Decimal,
}

struct TakerState {
    bias: Bias,
    remaining_cycle: u8,
}

impl Default for TakerState {
    fn default() -> Self {
        TakerState {
            bias: Bias::random_bias(),
            remaining_cycle: constants::TICKER_STATE_CYCLE,
        }
    }
}

enum Bias {
    Bullish,
    Bearish,
    Neutral,
}

impl Bias {
    fn to_dec(&self) -> Decimal {
        match self {
            Bias::Bearish => dec!(0.3),
            Bias::Bullish => dec!(0.7),
            Bias::Neutral => dec!(0.5),
        }
    }

    fn to_bias(n: u8) -> Option<Bias> {
        match n {
            1 => Some(Bias::Bearish),
            2 => Some(Bias::Bullish),
            3 => Some(Bias::Neutral),
            _ => None,
        }
    }

    fn random_bias() -> Self {
        let num = random_number(1, 3);
        Self::to_bias(num as u8).unwrap_or(Bias::Bullish)
    }

    fn buy_size(&self) -> (Decimal, Decimal) {
        match self {
            Bias::Bullish => (dec!(0.6), dec!(1.0)),
            Bias::Neutral => (dec!(0.3), dec!(0.6)),
            Bias::Bearish => (dec!(0.1), dec!(0.3)),
        }
    }

    fn sell_size(&self) -> (Decimal, Decimal) {
        match self {
            Bias::Bullish => (dec!(0.1), dec!(0.3)),
            Bias::Neutral => (dec!(0.3), dec!(0.6)),
            Bias::Bearish => (dec!(0.6), dec!(1.0)),
        }
    }
}

impl std::fmt::Display for Bias {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Bias::Bearish => write!(f, "Bearish"),
            Bias::Bullish => write!(f, "Bullish"),
            Bias::Neutral => write!(f, "Neutral"),
        }
    }
}

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

impl<T: ExchangeClient> Bot<T> {
    pub fn new(config: Config, client: T, role: BotRole, price_service: Arc<PriceService>) -> Self {
        Bot {
            client,
            config,
            taker_state: if role == BotRole::Taker {
                Some(TakerState::default())
            } else {
                None
            },
            price_service,
            backoff_until: None,
        }
    }

    pub async fn run(&mut self) {
        // login
        if let Err(e) = self
            .client
            .login(&self.config.email, &self.config.password)
            .await
        {
            tracing::error!(error = %e, "Failed to login, exiting");
            return;
        }

        tracing::info!(email = %self.config.email, "Bot logged in successfully");

        // taker: cancel any stale resting orders from previous runs
        if self.taker_state.is_some() {
            self.cancel_all_open_orders().await;
        }

        loop {
            if let Err(e) = self.cycle().await {
                tracing::error!(error = %e, "Cycle failed, skipping");
            }
            tokio::time::sleep(Duration::from_secs(self.config.interval_secs)).await;
        }
    }

    async fn cycle(&mut self) -> Result<()> {
        if let Some(instant) = self.backoff_until {
            if instant.elapsed() < Duration::from_secs(30) {
                tracing::warn!("Bot in backoff, skipping cycle");
                return Ok(());
            } else {
                self.backoff_until = None;
            }
        }

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

        for pair in pairs {
            let symbol = pair.symbol.as_str();
            let base_asset = pair.base_asset.as_str();
            let quote_asset = pair.quote_asset.as_str();

            let mid_price = match self.get_mid_price(symbol, base_asset).await {
                Some(price) => price,
                None => {
                    tracing::warn!(symbol = %symbol, "No price available, skipping pair");
                    continue;
                }
            };

            // re-fetch per pair to get accurate post-trade balances
            let pair_balance = match self.ensure_balance(base_asset, quote_asset).await {
                Ok(Some(b)) => b,
                _ => {
                    tracing::warn!(symbol = %symbol, "Balance check failed, skipping pair");
                    continue;
                }
            };

            match self.config.role {
                BotRole::Maker => self.maker_cycle(symbol, mid_price, &pair_balance).await?,
                BotRole::Taker => self.taker_cycle(symbol, &pair_balance).await?,
            }
        }

        // Decrement remaining_cycle once per full cycle, not per pair
        if let Some(state) = self.taker_state.as_mut() {
            if state.remaining_cycle == 0 {
                state.bias = Bias::random_bias();
                state.remaining_cycle = constants::TICKER_STATE_CYCLE;
                tracing::info!(bias = %state.bias, "Bias rotated");
            } else {
                state.remaining_cycle -= 1;
            }
        }

        Ok(())
    }

    async fn maker_cycle(
        &mut self,
        symbol: &str,
        mid_price: Decimal,
        available_balance: &PairBalance,
    ) -> Result<()> {
        // Safety guard — maker_cycle should never run for a taker bot
        // TODO: Investigate why the taker is able to place limit orders
        if self.config.role != BotRole::Maker {
            tracing::error!("maker_cycle called on non-maker bot, this is a bug");
            return Ok(());
        }

        let open_orders = match self.cancel_stale_orders(symbol, mid_price).await? {
            Some(orders) => orders,
            None => return Ok(()),
        };

        let bids_count = open_orders
            .iter()
            .filter(|o| o.side == OrderSide::Buy)
            .count();
        let asks_count = open_orders.len() - bids_count;

        let order_cap = self.config.order_cap.unwrap_or_else(|| {
            tracing::warn!(
                cap = constants::DEFAULT_ORDER_CAP,
                "Order cap not set using default value"
            );
            constants::DEFAULT_ORDER_CAP
        });

        let spread = self.config.spread.unwrap_or_else(|| {
            tracing::warn!(spread = %constants::DEFAULT_SPREAD, "Spread not set using default value");
            constants::DEFAULT_SPREAD
        });

        if bids_count < order_cap as usize {
            let bid_price = mid_price * (dec!(1) - spread);
            let quantity = random_base_quantity_to_buy(
                available_balance.quote_balance,
                bid_price,
                dec!(0.1),
                dec!(1),
            );

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
                match e {
                    ClientError::RateLimited => {
                        tracing::warn!(error = %e, "Place order rate limited, skipping cycle");
                        self.trigger_backoff();
                        return Ok(());
                    }
                    _ => tracing::warn!(error = %e, "Failed to place maker bid"),
                }
            } else {
                tracing::info!(symbol = %symbol, price = %bid_price, quantity = %quantity, "Placed maker bid");
            }
        } else {
            tracing::info!(symbol = %symbol, cap = self.config.order_cap, "Max maker Bids order cap reached");
        }

        if asks_count < order_cap as usize {
            let ask_price = mid_price * (dec!(1) + spread);
            let quantity = random_base_quantity_to_sell(
                available_balance.base_balance,
                ask_price,
                dec!(0.1),
                dec!(1),
            );

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
                match e {
                    ClientError::RateLimited => {
                        tracing::warn!(error = %e, "Place order rate limited, skipping cycle");
                        self.trigger_backoff();
                        return Ok(());
                    }
                    _ => tracing::warn!(error = %e, "Failed to place maker ask"),
                }
            } else {
                tracing::info!(symbol = %symbol, price = %ask_price, quantity = %quantity, "Placed maker ask");
            }
        } else {
            tracing::info!(symbol = %symbol, cap = self.config.order_cap, "Max maker Ask order cap reached");
        }

        Ok(())
    }

    async fn taker_cycle(&mut self, symbol: &str, available_balance: &PairBalance) -> Result<()> {
        // Safety guard
        if self.config.role != BotRole::Taker {
            tracing::error!("taker_cycle called on non-taker bot, this is a bug");
            return Ok(());
        }

        let orderbook = match try_call!(self, self.client.get_orderbook(symbol)) {
            Ok(o) => o,
            Err(e) => {
                tracing::warn!(symbol = %symbol, error = %e, "Failed to get orderbook");
                return Ok(());
            }
        };

        if let Some(ticker_state) = self.taker_state.as_mut() {
            tracing::info!(bias = %ticker_state.bias, remaining_cycle = %ticker_state.remaining_cycle, "Bias for this cycle");

            // Place taker buy or sell
            // the bias influences the probability of it being a buy
            // if Bullish => 70% chance of a buy AND 30% chance of sell
            // if Bearish => 30% chance of a buy AND 70% chance of sell
            // if Neutral => 50%
            if random_decimal(dec!(0), dec!(1)) <= ticker_state.bias.to_dec() {
                let best_ask = match orderbook.asks.first() {
                    Some(level) => level.price,
                    None => {
                        tracing::warn!(symbol = %symbol, "Ask side empty, skipping taker buy");
                        return Ok(());
                    }
                };

                // for Bullish => buy size is more to eat through multiple ask level
                // for Bearish => buy size is less
                // Neutral is neutral
                let price = best_ask;
                let (min, max) = ticker_state.bias.buy_size();
                let quantity = random_quote_to_spend(available_balance.quote_balance, min, max);

                if let Err(e) = self
                    .client
                    .place_order(symbol, OrderSide::Buy, OrderType::Market, None, quantity)
                    .await
                {
                    match e {
                        ClientError::RateLimited => {
                            tracing::warn!(error = %e, "Place order rate limited, skipping cycle");
                            self.trigger_backoff();
                            return Ok(());
                        }
                        _ => tracing::warn!(error = %e, "Failed to place taker ask"),
                    }
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

                let price = best_bid;
                let (min, max) = ticker_state.bias.sell_size();
                let quantity =
                    random_base_quantity_to_sell(available_balance.base_balance, price, min, max);

                if let Err(e) = self
                    .client
                    .place_order(symbol, OrderSide::Sell, OrderType::Market, None, quantity)
                    .await
                {
                    match e {
                        ClientError::RateLimited => {
                            tracing::warn!(error = %e, "Place order rate limited, skipping cycle");
                            self.trigger_backoff();
                            return Ok(());
                        }
                        _ => tracing::warn!(error = %e, "Failed to place taker sell"),
                    }
                } else {
                    tracing::info!(symbol = %symbol, price = %price, quantity = %quantity, "Placed taker sell");
                }
            }
        } else {
            tracing::warn!("No ticker state found");
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
                        self.trigger_backoff();
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

    async fn get_mid_price(&mut self, symbol: &str, base_asset: &str) -> Option<Decimal> {
        match self.client.get_ticker(symbol).await {
            Ok(ticker) => return Some(ticker.last_price),
            Err(ClientError::Other(_)) => {
                tracing::warn!(symbol = %symbol, "Ticker not available, trying price service");
            }
            Err(e) => {
                tracing::warn!(symbol = %symbol, error = %e, "Ticker failed");
                return None;
            }
        }

        // get cached price from price service
        match self.price_service.get_price(base_asset) {
            Some(price) => return Some(price),
            None => {
                tracing::warn!(asset = %base_asset, "Price service empty, using hardcoded fallback");
                let price = match base_asset {
                    "BTC" => dec!(75000),
                    "ETH" => dec!(2500),
                    "SOL" => dec!(85),
                    _ => {
                        tracing::warn!(asset = %base_asset, "No fallback price available");
                        return None;
                    }
                };
                Some(price)
            }
        }
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

    async fn cancel_stale_orders(
        &mut self,
        symbol: &str,
        mid_price: Decimal,
    ) -> Result<Option<Vec<OrderResponse>>> {
        let open_orders = match try_call!(self, self.client.get_open_orders(&symbol)) {
            Ok(orders) => orders,
            Err(e) => {
                tracing::warn!(symbol = %symbol, error = %e, "Failed to fetch open orders, skipping cycle");
                return Ok(None);
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

            let stale_threshold = self.config.stale_threshold.unwrap_or_else(|| {
                tracing::warn!(stale_threshold = %constants::STALE_THRESHOLD , "stale_threshold not set using default value");
                constants::STALE_THRESHOLD
            });

            if drift > stale_threshold {
                if let Err(e) = self.client.cancel_order(order.id).await {
                    tracing::warn!(error = %e, order_id = %order.id, "Failed to cancel stale order");
                }
            } else {
                remaining_orders.push(order);
            }
        }

        Ok(Some(remaining_orders))
    }

    fn trigger_backoff(&mut self) {
        self.backoff_until = Some(Instant::now());
    }

    async fn cancel_all_open_orders(&mut self) {
        let pairs = match self.client.get_active_pairs().await {
            Ok(p) => p,
            Err(_) => return,
        };

        for pair in pairs {
            if let Ok(orders) = self.client.get_open_orders(&pair.symbol).await {
                for order in orders {
                    if let Err(e) = self.client.cancel_order(order.id).await {
                        tracing::warn!(error = %e, order_id = %order.id, "Failed to cancel order on startup");
                    } else {
                        tracing::info!(order_id = %order.id, "Cancelled stale order on startup");
                    }
                }
            }
        }
    }
}
