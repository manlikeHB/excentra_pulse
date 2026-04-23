use dotenvy::dotenv;
use rust_decimal::Decimal;
use std::env;

#[derive(Debug)]
pub struct Config {
    pub exchange_url: String,
    pub email: String,
    pub password: String,
    pub spread: Decimal,
    pub size_min: Decimal,
    pub size_max: Decimal,
    pub interval_secs: u64,
    pub taker_prob: f64,
    pub stale_threshold: Decimal,
    pub min_balance: Decimal,
    pub target_balance: Decimal,
    pub order_cap: u8,
}

impl Config {
    pub fn from_env() -> Config {
        dotenv().ok();

        let exchange_url = require_env("EXCHANGE_URL");
        let email = require_env("EMAIL");
        let password = require_env("PASSWORD");
        let spread = parse_env::<Decimal>("SPREAD");
        let size_min = parse_env::<Decimal>("SIZE_MIN");
        let size_max = parse_env::<Decimal>("SIZE_MAX");
        let interval_secs = parse_env::<u64>("INTERVAL_SECS");
        let taker_prob = parse_env::<f64>("TAKER_PROB");
        let stale_threshold = parse_env::<Decimal>("STALE_THRESHOLD");
        let min_balance = parse_env::<Decimal>("MIN_BALANCE");
        let target_balance = parse_env::<Decimal>("TARGET_BALANCE");
        let order_cap = parse_env::<u8>("ORDER_CAP");

        assert!(
            taker_prob > 0.0 && taker_prob < 1.0,
            "TAKER_PROB must be between 0 and 1"
        );
        assert!(size_min < size_max, "SIZE_MIN must be less than SIZE_MAX");
        assert!(
            min_balance < target_balance,
            "MIN_BALANCE must be less than TARGET_BALANCE"
        );

        Config {
            exchange_url,
            email,
            password,
            spread,
            size_min,
            size_max,
            interval_secs,
            taker_prob,
            stale_threshold,
            min_balance,
            target_balance,
            order_cap,
        }
    }
}

fn parse_env<T: std::str::FromStr>(key: &str) -> T
where
    T::Err: std::fmt::Debug,
{
    env::var(key)
        .unwrap_or_else(|_| panic!("{} must be set", key))
        .parse()
        .unwrap_or_else(|_| panic!("{key} must be a valid value"))
}

fn require_env(key: &str) -> String {
    env::var(key).unwrap_or_else(|_| panic!("{key} must be set"))
}
