use dotenvy::dotenv;
use rust_decimal::Decimal;
use std::{env, str::FromStr};

#[derive(Debug)]
pub struct Config {
    pub email: String,
    pub password: String,
    pub interval_secs: u64,
    pub role: BotRole,
    pub spread: Option<Decimal>,
    pub stale_threshold: Option<Decimal>,
    pub order_cap: Option<u8>,
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum BotRole {
    Taker,
    Maker,
}

impl std::fmt::Display for BotRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BotRole::Maker => write!(f, "maker"),
            BotRole::Taker => write!(f, "taker"),
        }
    }
}

impl Config {
    pub fn from_env() -> (String, Vec<Config>) {
        dotenv().ok();

        let exchange_url = require_env("EXCHANGE_URL");
        let bot_count = parse_env::<u8>("BOT_COUNT");

        let mut bot_configs = Vec::new();

        for i in 1..=bot_count {
            let role = parse_env::<BotRole>(&format!("BOT_{i}_ROLE"));

            match role {
                BotRole::Taker => {
                    let email = require_env(&format!("BOT_{i}_EMAIL"));
                    let password = require_env(&format!("BOT_{i}_PASSWORD"));
                    let interval_secs = parse_env::<u64>(&format!("BOT_{i}_INTERVAL_SECS"));

                    let config = Self::new(email, password, interval_secs, role, None, None, None);
                    bot_configs.push(config);
                }
                BotRole::Maker => {
                    let email = require_env(&format!("BOT_{i}_EMAIL"));
                    let password = require_env(&format!("BOT_{i}_PASSWORD"));
                    let spread = parse_env::<Decimal>(&format!("BOT_{i}_SPREAD"));
                    let interval_secs = parse_env::<u64>(&format!("BOT_{i}_INTERVAL_SECS"));
                    let stale_threshold = parse_env::<Decimal>(&format!("BOT_{i}_STALE_THRESHOLD"));

                    let order_cap = parse_env::<u8>("ORDER_CAP");

                    let config = Self::new(
                        email,
                        password,
                        interval_secs,
                        role,
                        Some(spread),
                        Some(stale_threshold),
                        Some(order_cap),
                    );
                    bot_configs.push(config);
                }
            }
        }

        (exchange_url, bot_configs)
    }

    fn new(
        email: String,
        password: String,
        interval_secs: u64,
        role: BotRole,
        spread: Option<Decimal>,
        stale_threshold: Option<Decimal>,
        order_cap: Option<u8>,
    ) -> Config {
        Config {
            email,
            password,
            interval_secs,
            role,
            spread,
            stale_threshold,
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

impl FromStr for BotRole {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "taker" => Ok(BotRole::Taker),
            "maker" => Ok(BotRole::Maker),
            other => Err(format!(
                "Invalid bot role `{other}`, expected: maker or taker"
            )),
        }
    }
}
