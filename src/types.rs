use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use uuid::Uuid;

#[derive(serde::Deserialize)]
pub struct LoginResponse {
    pub access_token: String,
}

#[derive(Debug, serde::Deserialize, Clone)]
pub struct TradingPairsResponse {
    pub id: Uuid,
    pub base_asset: String,
    pub quote_asset: String,
    pub symbol: String,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, serde::Deserialize)]
pub struct TickerResponse {
    pub symbol: String,
    pub last_price: Decimal,
    pub high_24h: Decimal,
    pub low_24h: Decimal,
    pub volume_24h: Decimal,
    pub price_change_pct: Decimal,
}

#[derive(Debug, serde::Deserialize)]
pub struct OrderBookResponse {
    pub symbol: String,
    pub bids: Vec<PriceLevelResponse>,
    pub asks: Vec<PriceLevelResponse>,
}

#[derive(Debug, serde::Deserialize)]
pub struct PriceLevelResponse {
    pub price: Decimal,
    pub quantity: Decimal,
}

#[derive(Debug, serde::Deserialize, PartialEq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum OrderSide {
    Buy,
    Sell,
}

#[derive(Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum OrderType {
    Market,
    Limit,
}

#[derive(Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum OrderStatus {
    Open,
    #[serde(rename = "partially_filled")]
    PartiallyFilled,
    Filled,
    Cancelled,
}

#[derive(Debug, serde::Deserialize)]
pub struct OrderResponse {
    pub id: Uuid,
    pub symbol: String,
    pub side: OrderSide,
    pub order_type: OrderType,
    pub price: Option<Decimal>,
    pub quantity: Decimal,
    pub remaining_quantity: Decimal,
    pub status: OrderStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, serde::Deserialize)]
pub struct PlaceOrderResponse {
    pub order_id: Uuid,
    pub status: OrderStatus,
    pub filled_quantity: Decimal,
    pub remaining_quantity: Decimal,
    pub trades: Vec<TradeInfo>,
}

#[derive(Debug, serde::Deserialize)]
pub struct TradeInfo {
    pub price: Decimal,
    pub quantity: Decimal,
}

#[derive(Debug, serde::Deserialize)]
pub struct BalanceResponse {
    pub asset: String,
    pub available: Decimal,
    pub held: Decimal,
}

#[derive(serde::Deserialize)]
pub struct PaginatedResponse<T> {
    pub data: Vec<T>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct AssetResponse {
    pub symbol: String,
    pub decimals: i16,
    pub is_active: bool,
    pub coingecko_id: Option<String>,
}
