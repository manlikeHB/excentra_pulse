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
        if let Some(until) = self.backoff_until {
            let now = Instant::now();

            if now < until {
                let remaining = until.duration_since(now);

                tracing::warn!(
                    "Bot in backoff, skipping cycle: {} mins remaining",
                    remaining.as_secs() / 60
                );

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
                    ClientError::RateLimited(secs) => {
                        tracing::warn!(error = %e, "Place order rate limited, skipping cycle");
                        self.trigger_backoff(secs);
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
                    ClientError::RateLimited(secs) => {
                        tracing::warn!(error = %e, "Place order rate limited, skipping cycle");
                        self.trigger_backoff(secs);
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
                        ClientError::RateLimited(secs) => {
                            tracing::warn!(error = %e, "Place order rate limited, skipping cycle");
                            self.trigger_backoff(secs);
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
                        ClientError::RateLimited(secs) => {
                            tracing::warn!(error = %e, "Place order rate limited, skipping cycle");
                            self.trigger_backoff(secs);
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
                    Err(ClientError::RateLimited(secs)) => {
                        tracing::warn!(asset = %asset, "Deposit rate limited, skipping cycle");
                        self.trigger_backoff(secs);
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
            Some(price) => Some(price),
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
        let open_orders = match try_call!(self, self.client.get_open_orders(symbol)) {
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

    fn trigger_backoff(&mut self, secs: u64) {
        tracing::warn!(secs = secs, "Rate limited, backing off");
        self.backoff_until = Some(Instant::now() + Duration::from_secs(secs));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::*;
    use chrono::Utc;
    use std::sync::{Arc, Mutex};
    use uuid::Uuid;

    // ─── MockClient ───────────────────────────────────────────────────────────

    #[derive(Clone)]
    struct MockClient {
        ticker: Option<TickerResponse>,
        orderbook: OrderBookResponse,
        open_orders: Vec<OrderResponse>,
        balances: Vec<BalanceResponse>,
        placed_orders: Arc<Mutex<Vec<(OrderSide, OrderType, Option<Decimal>, Decimal)>>>,
        cancelled_orders: Arc<Mutex<Vec<Uuid>>>,
        deposit_called: Arc<Mutex<Vec<String>>>,
    }

    impl MockClient {
        fn new() -> Self {
            Self {
                ticker: Some(TickerResponse {
                    symbol: "BTC/USDT".to_string(),
                    last_price: dec!(75000),
                    high_24h: dec!(76000),
                    low_24h: dec!(74000),
                    volume_24h: dec!(100),
                    price_change_pct: dec!(0.5),
                }),
                orderbook: OrderBookResponse {
                    symbol: "BTC/USDT".to_string(),
                    bids: vec![PriceLevelResponse {
                        price: dec!(74900),
                        quantity: dec!(0.1),
                    }],
                    asks: vec![PriceLevelResponse {
                        price: dec!(75100),
                        quantity: dec!(0.1),
                    }],
                },
                open_orders: vec![],
                balances: vec![
                    BalanceResponse {
                        asset: "BTC".to_string(),
                        available: dec!(0.1),
                        held: dec!(0),
                    },
                    BalanceResponse {
                        asset: "USDT".to_string(),
                        available: dec!(1000),
                        held: dec!(0),
                    },
                ],
                placed_orders: Arc::new(Mutex::new(vec![])),
                cancelled_orders: Arc::new(Mutex::new(vec![])),
                deposit_called: Arc::new(Mutex::new(vec![])),
            }
        }
    }

    impl ExchangeClient for MockClient {
        async fn login(&mut self, _email: &str, _password: &str) -> Result<(), ClientError> {
            Ok(())
        }

        async fn refresh(&mut self) -> Result<(), ClientError> {
            Ok(())
        }

        async fn get_active_pairs(&self) -> Result<Vec<TradingPairsResponse>, ClientError> {
            Ok(vec![TradingPairsResponse {
                id: Uuid::new_v4(),
                base_asset: "BTC".to_string(),
                quote_asset: "USDT".to_string(),
                symbol: "BTC/USDT".to_string(),
                is_active: true,
                created_at: Utc::now(),
            }])
        }

        async fn get_ticker(&self, _symbol: &str) -> Result<TickerResponse, ClientError> {
            match &self.ticker {
                Some(t) => Ok(TickerResponse {
                    symbol: t.symbol.clone(),
                    last_price: t.last_price,
                    high_24h: t.high_24h,
                    low_24h: t.low_24h,
                    volume_24h: t.volume_24h,
                    price_change_pct: t.price_change_pct,
                }),
                None => Err(ClientError::Other("No ticker".to_string())),
            }
        }

        async fn get_orderbook(&self, _symbol: &str) -> Result<OrderBookResponse, ClientError> {
            Ok(OrderBookResponse {
                symbol: self.orderbook.symbol.clone(),
                bids: self.orderbook.bids.clone(),
                asks: self.orderbook.asks.clone(),
            })
        }

        async fn get_open_orders(&self, _symbol: &str) -> Result<Vec<OrderResponse>, ClientError> {
            Ok(self.open_orders.clone())
        }

        async fn get_balances(&self) -> Result<Vec<BalanceResponse>, ClientError> {
            Ok(self.balances.clone())
        }

        async fn deposit(
            &self,
            asset: &str,
            _amount: Decimal,
        ) -> Result<BalanceResponse, ClientError> {
            self.deposit_called.lock().unwrap().push(asset.to_string());
            Ok(BalanceResponse {
                asset: asset.to_string(),
                available: dec!(1000),
                held: dec!(0),
            })
        }

        async fn place_order(
            &self,
            _symbol: &str,
            side: OrderSide,
            order_type: OrderType,
            price: Option<Decimal>,
            quantity: Decimal,
        ) -> Result<PlaceOrderResponse, ClientError> {
            self.placed_orders
                .lock()
                .unwrap()
                .push((side, order_type, price, quantity));
            Ok(PlaceOrderResponse {
                order_id: Uuid::new_v4(),
                status: OrderStatus::Open,
                filled_quantity: dec!(0),
                remaining_quantity: quantity,
                trades: vec![],
            })
        }

        async fn cancel_order(&self, id: Uuid) -> Result<(), ClientError> {
            self.cancelled_orders.lock().unwrap().push(id);
            Ok(())
        }

        async fn get_assets(&self) -> Result<Vec<AssetResponse>, ClientError> {
            Ok(vec![])
        }
    }

    // ─── Helpers ──────────────────────────────────────────────────────────────

    fn maker_config() -> Config {
        Config {
            email: "bot@test.com".to_string(),
            password: "password".to_string(),
            interval_secs: 10,
            role: BotRole::Maker,
            spread: Some(dec!(0.002)),
            stale_threshold: Some(dec!(0.005)),
            order_cap: Some(3),
        }
    }

    fn taker_config() -> Config {
        Config {
            email: "taker@test.com".to_string(),
            password: "password".to_string(),
            interval_secs: 10,
            role: BotRole::Taker,
            spread: None,
            stale_threshold: None,
            order_cap: None,
        }
    }

    fn price_service() -> Arc<PriceService> {
        Arc::new(PriceService::new(vec![]))
    }

    fn make_open_order(side: OrderSide, price: Decimal) -> OrderResponse {
        OrderResponse {
            id: Uuid::new_v4(),
            symbol: "BTC/USDT".to_string(),
            side,
            order_type: OrderType::Limit,
            price: Some(price),
            quantity: dec!(0.01),
            remaining_quantity: dec!(0.01),
            status: OrderStatus::Open,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    // ─── Maker Tests ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn maker_places_bid_and_ask_when_book_empty() {
        let client = MockClient::new();
        let placed = client.placed_orders.clone();

        let mut bot = Bot::new(maker_config(), client, BotRole::Maker, price_service());
        bot.maker_cycle(
            "BTC/USDT",
            dec!(75000),
            &PairBalance {
                base_balance: dec!(0.1),
                quote_balance: dec!(1000),
            },
        )
        .await
        .unwrap();

        let orders = placed.lock().unwrap();
        assert_eq!(orders.len(), 2);

        // first order is a limit buy
        assert_eq!(orders[0].0, OrderSide::Buy);
        assert!(matches!(orders[0].1, OrderType::Limit));
        assert!(orders[0].2.is_some()); // has a price

        // second order is a limit sell
        assert_eq!(orders[1].0, OrderSide::Sell);
        assert!(matches!(orders[1].1, OrderType::Limit));
        assert!(orders[1].2.is_some());
    }

    #[tokio::test]
    async fn maker_bid_price_is_below_mid() {
        let client = MockClient::new();
        let placed = client.placed_orders.clone();

        let mut bot = Bot::new(maker_config(), client, BotRole::Maker, price_service());
        let mid = dec!(75000);
        bot.maker_cycle(
            "BTC/USDT",
            mid,
            &PairBalance {
                base_balance: dec!(0.1),
                quote_balance: dec!(1000),
            },
        )
        .await
        .unwrap();

        let orders = placed.lock().unwrap();
        let bid_price = orders[0].2.unwrap();
        let ask_price = orders[1].2.unwrap();

        assert!(bid_price < mid, "bid should be below mid price");
        assert!(ask_price > mid, "ask should be above mid price");
    }

    #[tokio::test]
    async fn maker_skips_bid_when_cap_reached() {
        let mut client = MockClient::new();
        // fill bid side to cap (3)
        client.open_orders = vec![
            make_open_order(OrderSide::Buy, dec!(74850)),
            make_open_order(OrderSide::Buy, dec!(74850)),
            make_open_order(OrderSide::Buy, dec!(74850)),
        ];

        let placed = client.placed_orders.clone();

        let mut bot = Bot::new(maker_config(), client, BotRole::Maker, price_service());
        bot.maker_cycle(
            "BTC/USDT",
            dec!(75000),
            &PairBalance {
                base_balance: dec!(0.1),
                quote_balance: dec!(1000),
            },
        )
        .await
        .unwrap();

        let orders = placed.lock().unwrap();
        // only ask should be placed, bid is capped
        assert_eq!(orders.len(), 1);
        assert_eq!(orders[0].0, OrderSide::Sell);
    }

    #[tokio::test]
    async fn maker_skips_ask_when_cap_reached() {
        let mut client = MockClient::new();
        // fill ask side to cap (3)
        client.open_orders = vec![
            make_open_order(OrderSide::Sell, dec!(75150)),
            make_open_order(OrderSide::Sell, dec!(75150)),
            make_open_order(OrderSide::Sell, dec!(75150)),
        ];

        let placed = client.placed_orders.clone();

        let mut bot = Bot::new(maker_config(), client, BotRole::Maker, price_service());
        bot.maker_cycle(
            "BTC/USDT",
            dec!(75000),
            &PairBalance {
                base_balance: dec!(0.1),
                quote_balance: dec!(1000),
            },
        )
        .await
        .unwrap();

        let orders = placed.lock().unwrap();
        // only bid should be placed
        assert_eq!(orders.len(), 1);
        assert_eq!(orders[0].0, OrderSide::Buy);
    }

    #[tokio::test]
    async fn maker_cancels_stale_order_and_replaces() {
        let mut client = MockClient::new();
        // order placed at 70000, mid is now 75000 — drift = 6.7%, above threshold 0.5%
        client.open_orders = vec![make_open_order(OrderSide::Buy, dec!(70000))];

        let cancelled = client.cancelled_orders.clone();
        let placed = client.placed_orders.clone();

        let mut bot = Bot::new(maker_config(), client, BotRole::Maker, price_service());
        bot.maker_cycle(
            "BTC/USDT",
            dec!(75000),
            &PairBalance {
                base_balance: dec!(0.1),
                quote_balance: dec!(1000),
            },
        )
        .await
        .unwrap();

        // stale order was cancelled
        assert_eq!(cancelled.lock().unwrap().len(), 1);

        // new bid and ask placed
        assert_eq!(placed.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn maker_keeps_fresh_order_within_threshold() {
        let mut client = MockClient::new();
        // order at 74925 — drift from 75000 = 0.1%, below threshold 0.5%
        client.open_orders = vec![make_open_order(OrderSide::Buy, dec!(74925))];

        let cancelled = client.cancelled_orders.clone();

        let mut bot = Bot::new(maker_config(), client, BotRole::Maker, price_service());
        bot.maker_cycle(
            "BTC/USDT",
            dec!(75000),
            &PairBalance {
                base_balance: dec!(0.1),
                quote_balance: dec!(1000),
            },
        )
        .await
        .unwrap();

        // order should NOT be cancelled
        assert_eq!(cancelled.lock().unwrap().len(), 0);
    }

    // ─── Taker Tests ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn taker_places_market_buy_when_bullish() {
        let client = MockClient::new();
        let placed = client.placed_orders.clone();

        let mut bot = Bot::new(taker_config(), client, BotRole::Taker, price_service());
        if let Some(state) = bot.taker_state.as_mut() {
            state.bias = Bias::Bullish;
        }

        let mut got_buy = false;
        for _ in 0..20 {
            placed.lock().unwrap().clear();
            bot.taker_cycle(
                "BTC/USDT",
                &PairBalance {
                    base_balance: dec!(0.1),
                    quote_balance: dec!(1000),
                },
            )
            .await
            .unwrap();

            let orders = placed.lock().unwrap();
            if !orders.is_empty() && orders[0].0 == OrderSide::Buy {
                got_buy = true;
                assert!(matches!(orders[0].1, OrderType::Market));
                assert!(orders[0].2.is_none());
                break;
            }
        }
        assert!(
            got_buy,
            "expected at least one market buy with Bullish bias"
        );
    }

    #[tokio::test]
    async fn taker_places_market_sell_when_bearish() {
        let client = MockClient::new();
        let placed = client.placed_orders.clone();

        let mut bot = Bot::new(taker_config(), client, BotRole::Taker, price_service());

        // force bearish bias — sell path always taken (random_decimal always > 0.3)
        // we set bias to Bearish and override the roll by making remaining_cycle 0
        // to ensure the bearish path. Since random is involved, we just verify
        // that when we force sell side, a market sell is placed.
        if let Some(state) = bot.taker_state.as_mut() {
            state.bias = Bias::Bearish;
        }

        // run multiple times — with Bearish (0.3 threshold), sell path triggers ~70%
        // we just need one sell to confirm the path works
        let mut got_sell = false;
        for _ in 0..20 {
            placed.lock().unwrap().clear();
            bot.taker_cycle(
                "BTC/USDT",
                &PairBalance {
                    base_balance: dec!(0.1),
                    quote_balance: dec!(1000),
                },
            )
            .await
            .unwrap();

            let orders = placed.lock().unwrap();
            if !orders.is_empty() && orders[0].0 == OrderSide::Sell {
                got_sell = true;
                assert!(matches!(orders[0].1, OrderType::Market));
                assert!(orders[0].2.is_none());
                break;
            }
        }
        assert!(
            got_sell,
            "expected at least one market sell with Bearish bias"
        );
    }

    #[tokio::test]
    async fn taker_skips_buy_when_asks_empty() {
        let mut client = MockClient::new();
        client.orderbook.asks = vec![];

        let placed = client.placed_orders.clone();

        let mut bot = Bot::new(taker_config(), client, BotRole::Taker, price_service());
        if let Some(state) = bot.taker_state.as_mut() {
            state.bias = Bias::Bullish;
        }

        // run multiple times to ensure the buy path is triggered at least once
        let mut buy_path_skipped = false;
        for _ in 0..20 {
            placed.lock().unwrap().clear();
            bot.taker_cycle(
                "BTC/USDT",
                &PairBalance {
                    base_balance: dec!(0.1),
                    quote_balance: dec!(1000),
                },
            )
            .await
            .unwrap();

            let orders = placed.lock().unwrap();
            // if no order placed, buy path was triggered and correctly skipped
            if orders.is_empty() {
                buy_path_skipped = true;
                break;
            }
            // if a sell was placed, that's fine — sell path triggered, not what we're testing
        }

        assert!(
            buy_path_skipped,
            "expected buy to be skipped when asks are empty"
        );
    }

    #[tokio::test]
    async fn taker_skips_sell_when_bids_empty() {
        let mut client = MockClient::new();
        client.orderbook.bids = vec![]; // empty bid side

        let placed = client.placed_orders.clone();

        let mut bot = Bot::new(taker_config(), client, BotRole::Taker, price_service());
        if let Some(state) = bot.taker_state.as_mut() {
            state.bias = Bias::Bearish; // force sell path
        }

        // run until we hit a sell attempt
        for _ in 0..20 {
            placed.lock().unwrap().clear();
            bot.taker_cycle(
                "BTC/USDT",
                &PairBalance {
                    base_balance: dec!(0.1),
                    quote_balance: dec!(1000),
                },
            )
            .await
            .unwrap();

            // if sell path was triggered, nothing should be placed
            let orders = placed.lock().unwrap();
            assert!(
                orders.is_empty() || orders[0].0 == OrderSide::Buy,
                "sell should be skipped with empty bids"
            );
        }
    }

    // ─── Balance / Deposit Tests ──────────────────────────────────────────────

    #[tokio::test]
    async fn deposit_triggered_when_balance_below_minimum() {
        let mut client = MockClient::new();
        // BTC balance below minimum (min = 0.05 / 10 = 0.005)
        client.balances = vec![
            BalanceResponse {
                asset: "BTC".to_string(),
                available: dec!(0.001), // below min
                held: dec!(0),
            },
            BalanceResponse {
                asset: "USDT".to_string(),
                available: dec!(1000),
                held: dec!(0),
            },
        ];

        let deposit_called = client.deposit_called.clone();

        let mut bot = Bot::new(maker_config(), client, BotRole::Maker, price_service());
        bot.ensure_balance("BTC", "USDT").await.unwrap();

        let deposited = deposit_called.lock().unwrap();
        assert!(deposited.contains(&"BTC".to_string()));
    }

    #[tokio::test]
    async fn no_deposit_when_balance_above_minimum() {
        let client = MockClient::new(); // default balances are above minimum

        let deposit_called = client.deposit_called.clone();

        let mut bot = Bot::new(maker_config(), client, BotRole::Maker, price_service());
        bot.ensure_balance("BTC", "USDT").await.unwrap();

        assert!(deposit_called.lock().unwrap().is_empty());
    }

    // ─── Backoff Tests ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn cycle_skips_when_in_backoff() {
        let client = MockClient::new();
        let placed = client.placed_orders.clone();

        let mut bot = Bot::new(maker_config(), client, BotRole::Maker, price_service());

        // trigger backoff for 60 seconds
        bot.trigger_backoff(60);

        bot.cycle().await.unwrap();

        // no orders should have been placed
        assert!(placed.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn cycle_resumes_after_backoff_expires() {
        let client = MockClient::new();
        let placed = client.placed_orders.clone();

        let mut bot = Bot::new(maker_config(), client, BotRole::Maker, price_service());

        // set backoff to already expired (1 nanosecond ago)
        bot.backoff_until = Some(Instant::now() - Duration::from_nanos(1));

        bot.cycle().await.unwrap();

        // backoff cleared and orders placed
        assert!(bot.backoff_until.is_none());
        assert!(!placed.lock().unwrap().is_empty());
    }

    // ─── Price Resolution Tests ───────────────────────────────────────────────

    #[tokio::test]
    async fn get_mid_price_uses_ticker_when_available() {
        let client = MockClient::new(); // ticker returns 75000

        let mut bot = Bot::new(maker_config(), client, BotRole::Maker, price_service());
        let price = bot.get_mid_price("BTC/USDT", "BTC").await;

        assert_eq!(price, Some(dec!(75000)));
    }

    #[tokio::test]
    async fn get_mid_price_falls_back_to_hardcoded_when_no_ticker() {
        let mut client = MockClient::new();
        client.ticker = None; // no ticker available

        let mut bot = Bot::new(maker_config(), client, BotRole::Maker, price_service());
        let price = bot.get_mid_price("BTC/USDT", "BTC").await;

        // hardcoded fallback for BTC
        assert_eq!(price, Some(dec!(75000)));
    }

    #[tokio::test]
    async fn taker_never_places_limit_orders() {
        let client = MockClient::new();
        let placed = client.placed_orders.clone();

        let mut bot = Bot::new(taker_config(), client, BotRole::Taker, price_service());

        // run many cycles to cover both buy and sell paths
        for _ in 0..1000 {
            bot.taker_cycle(
                "BTC/USDT",
                &PairBalance {
                    base_balance: dec!(0.1),
                    quote_balance: dec!(1000),
                },
            )
            .await
            .unwrap();
        }

        let orders = placed.lock().unwrap();
        for order in orders.iter() {
            assert!(
                matches!(order.1, OrderType::Market),
                "taker placed a non-market order: {:?}",
                order.1
            );
        }
    }
}
