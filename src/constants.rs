use rust_decimal::{Decimal, dec};

pub const DEFAULT_ORDER_CAP: u8 = 10;
pub const DEFAULT_SPREAD: Decimal = dec!(0.003);
pub const TICKER_STATE_CYCLE: u8 = 15;
pub const STALE_THRESHOLD: Decimal = dec!(0.005);
