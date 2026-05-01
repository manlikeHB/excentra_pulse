use std::sync::Arc;

use anyhow::Result;
use excentra_pulse::{
    bot::Bot,
    client::{Client, ExchangeClient},
    config::Config,
    price_service::PriceService,
};
use tracing::Instrument;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().init();

    let (exchange_url, configs) = Config::from_env();
    let base_url = format!("{}/api/v1", &exchange_url);
    let client = Client::new(&base_url);

    let assets = match client.get_assets().await {
        Ok(assets) => assets,
        Err(e) => {
            tracing::error!(error = %e, "Failed to fetch assets, exiting");
            anyhow::bail!("failed to fetch assets");
        }
    };

    drop(client);

    let price_service = Arc::new(PriceService::new(assets));
    let ps = price_service.clone();
    tokio::spawn(async move {
        ps.run_and_update_prices().await;
    });

    let mut handles_vec = Vec::new();
    for (idx, config) in configs.into_iter().enumerate() {
        let role = config.role;
        let price_service = price_service.clone();
        let base_url = format!("{}/api/v1", &exchange_url);
        let bot_client = Client::new(&base_url);
        let mut bot = Bot::new(config, bot_client, role, price_service.clone());

        let span = tracing::info_span!("Bot", bot_id = idx + 1, role = %role);

        let handle = tokio::spawn(async move { bot.run().await }.instrument(span));

        handles_vec.push(handle);
    }

    for handle in handles_vec {
        handle.await?
    }

    Ok(())
}
