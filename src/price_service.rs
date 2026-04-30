use anyhow::Result;
use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
    time::Duration,
};

use rust_decimal::{Decimal, prelude::FromPrimitive};

use crate::{client::http_get_external, types::AssetResponse};

#[derive(Debug, Clone)]
pub struct PriceService {
    prices: Arc<RwLock<HashMap<String, Decimal>>>, // asset symbol -> price
    assets: HashMap<String, String>,               // coingecko_id -> symbol
}

impl PriceService {
    pub fn new(assets: Vec<AssetResponse>) -> Self {
        PriceService {
            prices: Arc::new(RwLock::new(HashMap::new())),
            assets: assets
                .into_iter()
                .filter(|a| a.symbol != "USDT".to_string())
                .filter(|a| a.coingecko_id.is_some())
                .map(|a| (a.coingecko_id.unwrap(), a.symbol.to_owned()))
                .collect(),
        }
    }

    pub fn get_price(&self, asset: &str) -> Option<Decimal> {
        self.prices.read().unwrap().get(asset).copied()
    }

    pub fn price_handle(&self) -> Arc<RwLock<HashMap<String, Decimal>>> {
        Arc::clone(&self.prices)
    }

    pub async fn fetch_and_update_prices(&self) {
        let ids: Vec<String> = self.assets.keys().cloned().collect();

        match fetch_coingecko_price(ids).await {
            Ok(res) => {
                for (coingecko_id, price) in res {
                    if let Some(asset) = self.assets.get(&coingecko_id) {
                        self.prices.write().unwrap().insert(asset.clone(), price);
                        tracing::info!(asset = %asset, price = %price, "Asset price updated");
                    }
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "Failed to fetch coingecko price");
            }
        }
    }

    pub async fn run_and_update_prices(&self) {
        self.fetch_and_update_prices().await;
        loop {
            tokio::time::sleep(Duration::from_secs(60 * 60 * 24)).await;
            self.fetch_and_update_prices().await;
        }
    }
}

async fn fetch_coingecko_price(coingecko_ids: Vec<String>) -> Result<Vec<(String, Decimal)>> {
    let ids = coingecko_ids.join(",");
    let url = format!(
        "https://api.coingecko.com/api/v3/simple/price?ids={}&vs_currencies=usd",
        ids
    );

    let res: HashMap<String, serde_json::Value> = http_get_external(&url).await?;

    let mut prices = vec![];
    for (key, value) in res {
        let price_f64 = value
            .get("usd")
            .and_then(|v| v.as_f64())
            .ok_or_else(|| anyhow::anyhow!("No price in CoinGecko response"))?;

        let price_dec = Decimal::from_f64(price_f64)
            .ok_or_else(|| anyhow::anyhow!("Failed to convert CoinGecko price to Decimal"))?;

        prices.push((key, price_dec));
    }

    Ok(prices)
}
