use crate::types::*;
use anyhow::{Ok, Result};
use reqwest::header::AUTHORIZATION;
use rust_decimal::Decimal;
use uuid::Uuid;

pub struct Client {
    http: reqwest::Client,
    base_url: String,
    access_token: Option<String>,
}

impl Client {
    pub fn new(base_url: &str) -> Self {
        let http = reqwest::ClientBuilder::new()
            .cookie_store(true)
            .build()
            .expect("Failed to build HTTP client");

        Client {
            http,
            base_url: base_url.to_string(),
            access_token: None,
        }
    }

    pub async fn login(&mut self, email: &str, password: &str) -> Result<()> {
        let res = self
            .http
            .post(format!("{}/auth/login", self.base_url))
            .json(&serde_json::json!({"email": email, "password": password}))
            .send()
            .await?;

        if !res.status().is_success() {
            anyhow::bail!("Login failed with status: {}", res.status());
        }

        let login_response: LoginResponse = res.json().await?;

        self.access_token = Some(login_response.access_token);

        Ok(())
    }

    pub async fn refresh(&mut self) -> Result<()> {
        let res = self
            .http
            .post(format!("{}/auth/refresh", self.base_url))
            .send()
            .await?;

        if !res.status().is_success() {
            anyhow::bail!("Token refresh failed with status: {}", res.status());
        }

        let login_response: LoginResponse = res.json().await?;
        self.access_token = Some(login_response.access_token);

        Ok(())
    }

    pub async fn get_active_pairs(&self) -> Result<Vec<TradingPairsResponse>> {
        let res = self
            .http
            .get(format!("{}/pairs/active", self.base_url))
            .send()
            .await?;

        if !res.status().is_success() {
            anyhow::bail!("Get active pairs failed with status: {}", res.status());
        }

        Ok(res.json().await?)
    }

    pub async fn get_ticker(&self, symbol: &str) -> Result<TickerResponse> {
        let res = self
            .http
            .get(format!(
                "{}/ticker/{}",
                self.base_url,
                to_path_symbol(symbol)
            ))
            .send()
            .await?;

        if !res.status().is_success() {
            anyhow::bail!("Get {symbol} ticker failed with status: {}", res.status());
        }

        Ok(res.json().await?)
    }

    pub async fn get_orderbook(&self, symbol: &str) -> Result<OrderBookResponse> {
        let res = self
            .http
            .get(format!(
                "{}/orderbook/{}",
                self.base_url,
                to_path_symbol(symbol)
            ))
            .send()
            .await?;

        if !res.status().is_success() {
            anyhow::bail!(
                "Get {symbol} orderbook failed with status: {}",
                res.status()
            );
        }

        Ok(res.json().await?)
    }

    pub async fn get_orders(&self, symbol: &str) -> Result<Vec<OrderResponse>> {
        let res = self
            .http
            .get(format!(
                "{}/orders?status=open,partially_filled&pair={}",
                self.base_url,
                to_path_symbol(symbol)
            ))
            .header(AUTHORIZATION, self.auth_header()?)
            .send()
            .await?;

        if !res.status().is_success() {
            anyhow::bail!("Get {symbol} orders failed with status: {}", res.status());
        }

        let paginated_res: PaginatedResponse<OrderResponse> = res.json().await?;

        Ok(paginated_res.data)
    }

    pub async fn get_balances(&self) -> Result<Vec<BalanceResponse>> {
        let res = self
            .http
            .get(format!("{}/balances", self.base_url))
            .header(AUTHORIZATION, self.auth_header()?)
            .send()
            .await?;

        if !res.status().is_success() {
            anyhow::bail!("failed to get balances with status: {}", res.status());
        }

        Ok(res.json().await?)
    }

    pub async fn deposit(&self, asset: &str, amount: Decimal) -> Result<BalanceResponse> {
        let res = self
            .http
            .post(format!("{}/balances/deposit", self.base_url))
            .json(&serde_json::json!({"asset": asset, "amount": amount.to_string()}))
            .header(AUTHORIZATION, self.auth_header()?)
            .send()
            .await?;

        if !res.status().is_success() {
            anyhow::bail!("failed to deposit {asset} with status: {}", res.status());
        }

        Ok(res.json().await?)
    }

    pub async fn place_order(
        &self,
        symbol: &str,
        side: OrderSide,
        order_type: OrderType,
        price: Option<Decimal>,
        quantity: Decimal,
    ) -> Result<OrderResponse> {
        let res = self
            .http
            .post(format!("{}/orders", self.base_url)).json(&serde_json::json!({
                "symbol": symbol, 
                "side": side, 
                "order_type": order_type, 
                "price": price.map(|p| p.to_string()), 
                "quantity": quantity.to_string() 
            })).header(AUTHORIZATION, self.auth_header()?)
            .send()
            .await?;

        if !res.status().is_success() {
            anyhow::bail!(
                "failed to place {symbol} order with status: {}",
                res.status()
            );
        }

        Ok(res.json().await?)
    }

    pub async fn cancel_order(&self, id: Uuid) -> Result<()> {
        let res = self
            .http
            .delete(format!("{}/orders/{}", self.base_url, id))
            .header(AUTHORIZATION, self.auth_header()?)
            .send()
            .await?;

        if !res.status().is_success() {
            anyhow::bail!("failed to cancel order with status: {}", res.status());
        }

        Ok(())
    }

    fn auth_header(&self) -> Result<String> {
        let token = self
            .access_token
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("Not logged in — access token missing"))?;
        Ok(format!("Bearer {}", token))
    }
}

fn to_path_symbol(symbol: &str) -> String {
    symbol.replace("/", "-")
}
