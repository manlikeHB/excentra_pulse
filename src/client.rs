use crate::types::*;
use anyhow::Result;
use reqwest::header::AUTHORIZATION;
use rust_decimal::Decimal;
use std::collections::HashMap;
use uuid::Uuid;

pub struct Client {
    http: reqwest::Client,
    base_url: String,
    access_token: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("Unauthorized")]
    Unauthorized,
    #[error("Rate limited")]
    RateLimited,
    #[error("{0}")]
    Other(String),
    #[error(transparent)]
    Network(#[from] reqwest::Error),
    #[error(transparent)]
    Parse(#[from] serde_json::Error),
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

    pub async fn login(&mut self, email: &str, password: &str) -> Result<(), ClientError> {
        let res = self
            .http
            .post(format!("{}/auth/login", self.base_url))
            .json(&serde_json::json!({"email": email, "password": password}))
            .send()
            .await?;

        if !res.status().is_success() {
            match res.status().as_u16() {
                401 => return Err(ClientError::Unauthorized),
                429 => return Err(ClientError::RateLimited),
                _ => {
                    return Err(ClientError::Other(format!(
                        "Request failed with status {}: {}",
                        res.status(),
                        res.text().await.unwrap_or_default()
                    )));
                }
            }
        }

        let login_response: LoginResponse = res.json().await?;

        self.access_token = Some(login_response.access_token);

        Ok(())
    }

    pub async fn refresh(&mut self) -> Result<(), ClientError> {
        let res = self
            .http
            .post(format!("{}/auth/refresh", self.base_url))
            .send()
            .await?;

        if !res.status().is_success() {
            match res.status().as_u16() {
                401 => return Err(ClientError::Unauthorized),
                429 => return Err(ClientError::RateLimited),
                _ => {
                    return Err(ClientError::Other(format!(
                        "Request failed with status {}: {}",
                        res.status(),
                        res.text().await.unwrap_or_default()
                    )));
                }
            }
        }

        let login_response: LoginResponse = res.json().await?;
        self.access_token = Some(login_response.access_token);

        Ok(())
    }

    pub async fn get_active_pairs(&self) -> Result<Vec<TradingPairsResponse>, ClientError> {
        let res = self
            .http
            .get(format!("{}/pairs/active", self.base_url))
            .send()
            .await?;

        if !res.status().is_success() {
            match res.status().as_u16() {
                401 => return Err(ClientError::Unauthorized),
                429 => return Err(ClientError::RateLimited),
                _ => {
                    return Err(ClientError::Other(format!(
                        "Request failed with status {}: {}",
                        res.status(),
                        res.text().await.unwrap_or_default()
                    )));
                }
            }
        }

        Ok(res.json().await?)
    }

    pub async fn get_ticker(&self, symbol: &str) -> Result<TickerResponse, ClientError> {
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
            match res.status().as_u16() {
                401 => return Err(ClientError::Unauthorized),
                429 => return Err(ClientError::RateLimited),
                _ => {
                    return Err(ClientError::Other(format!(
                        "Request failed with status {}: {}",
                        res.status(),
                        res.text().await.unwrap_or_default()
                    )));
                }
            }
        }

        Ok(res.json().await?)
    }

    pub async fn get_orderbook(&self, symbol: &str) -> Result<OrderBookResponse, ClientError> {
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
            match res.status().as_u16() {
                401 => return Err(ClientError::Unauthorized),
                429 => return Err(ClientError::RateLimited),
                _ => {
                    return Err(ClientError::Other(format!(
                        "Request failed with status {}: {}",
                        res.status(),
                        res.text().await.unwrap_or_default()
                    )));
                }
            }
        }

        Ok(res.json().await?)
    }

    pub async fn get_open_orders(&self, symbol: &str) -> Result<Vec<OrderResponse>, ClientError> {
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
            match res.status().as_u16() {
                401 => return Err(ClientError::Unauthorized),
                429 => return Err(ClientError::RateLimited),
                _ => {
                    return Err(ClientError::Other(format!(
                        "Request failed with status {}: {}",
                        res.status(),
                        res.text().await.unwrap_or_default()
                    )));
                }
            }
        }

        let paginated_res: PaginatedResponse<OrderResponse> = res.json().await?;

        Ok(paginated_res.data)
    }

    pub async fn get_balances(&self) -> Result<Vec<BalanceResponse>, ClientError> {
        let res = self
            .http
            .get(format!("{}/balances", self.base_url))
            .header(AUTHORIZATION, self.auth_header()?)
            .send()
            .await?;

        if !res.status().is_success() {
            match res.status().as_u16() {
                401 => return Err(ClientError::Unauthorized),
                429 => return Err(ClientError::RateLimited),
                _ => {
                    return Err(ClientError::Other(format!(
                        "Request failed with status {}: {}",
                        res.status(),
                        res.text().await.unwrap_or_default()
                    )));
                }
            }
        }

        Ok(res.json().await?)
    }

    pub async fn deposit(
        &self,
        asset: &str,
        amount: Decimal,
    ) -> Result<BalanceResponse, ClientError> {
        let res = self
            .http
            .post(format!("{}/balances/deposit", self.base_url))
            .json(&serde_json::json!({"asset": asset, "amount": amount.to_string()}))
            .header(AUTHORIZATION, self.auth_header()?)
            .send()
            .await?;

        if !res.status().is_success() {
            match res.status().as_u16() {
                401 => return Err(ClientError::Unauthorized),
                429 => return Err(ClientError::RateLimited),
                _ => {
                    return Err(ClientError::Other(format!(
                        "Request failed with status {}: {}",
                        res.status(),
                        res.text().await.unwrap_or_default()
                    )));
                }
            }
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
    ) -> Result<PlaceOrderResponse, ClientError> {
        let res = self
            .http
            .post(format!("{}/orders", self.base_url))
            .json(&serde_json::json!({
                "symbol": symbol,
                "side": side,
                "order_type": order_type,
                "price": price.map(|p| p.to_string()),
                "quantity": quantity.to_string()
            }))
            .header(AUTHORIZATION, self.auth_header()?)
            .send()
            .await?;

        if !res.status().is_success() {
            match res.status().as_u16() {
                401 => return Err(ClientError::Unauthorized),
                429 => return Err(ClientError::RateLimited),
                _ => {
                    return Err(ClientError::Other(format!(
                        "Request failed with status {}: {}",
                        res.status(),
                        res.text().await.unwrap_or_default()
                    )));
                }
            }
        }

        let response = match res.json::<PlaceOrderResponse>().await {
            Ok(o) => o,
            Err(e) => panic!("Failing to deserialize: {e:?}")
        };
        Ok(response)
        // Ok(res.json().await?)
    }

    pub async fn cancel_order(&self, id: Uuid) -> Result<(), ClientError> {
        let res = self
            .http
            .delete(format!("{}/orders/{}", self.base_url, id))
            .header(AUTHORIZATION, self.auth_header()?)
            .send()
            .await?;

        if !res.status().is_success() {
            match res.status().as_u16() {
                401 => return Err(ClientError::Unauthorized),
                429 => return Err(ClientError::RateLimited),
                _ => {
                    return Err(ClientError::Other(format!(
                        "Request failed with status {}: {}",
                        res.status(),
                        res.text().await.unwrap_or_default()
                    )));
                }
            }
        }

        Ok(())
    }

    pub async fn get_assets(&self) -> Result<Vec<AssetResponse>, ClientError> {
        let res = self
            .http
            .get(format!("{}/assets", self.base_url))
            .send()
            .await?;

        if !res.status().is_success() {
            return Err(match res.status().as_u16() {
                401 => ClientError::Unauthorized,
                429 => ClientError::RateLimited,
                _ => ClientError::Other(format!(
                    "Request failed with status {}: {}",
                    res.status(),
                    res.text().await.unwrap_or_default()
                )),
            });
        }

        Ok(res.json().await?)
    }

    fn auth_header(&self) -> Result<String, ClientError> {
        let token = self
            .access_token
            .as_deref()
            .ok_or(ClientError::Unauthorized)?;
        Ok(format!("Bearer {}", token))
    }

    pub async fn http_get_external(
        &self,
        url: &str,
    ) -> Result<HashMap<String, serde_json::Value>, ClientError> {
        let res = self
            .http
            .get(url)
            .header("User-Agent", "Excentra-Pulse/1.0")
            .send()
            .await?;

        if !res.status().is_success() {
            return Err(ClientError::Other(format!(
                "External request failed: {}",
                res.status()
            )));
        }

        Ok(res.json().await?)
    }
}

fn to_path_symbol(symbol: &str) -> String {
    symbol.replace("/", "-")
}
