use rust_decimal::{Decimal, dec};

// Deposit targets per asset — set at the exchange's per-request deposit cap.
// Hardcoded temporarily. When new assets are added to the exchange, add them here.
// See: BalanceRequest::validate_deposit in the exchange codebase.
pub fn max_deposit(asset: &str) -> Decimal {
    match asset {
        "USDT" => dec!(1000),
        "BTC" => dec!(0.05),
        "ETH" => dec!(0.5),
        "SOL" => dec!(5),
        _ => {
            tracing::warn!(asset = %asset, "Unknown asset, no deposit target defined");
            dec!(0)
        }
    }
}

// compute minimum balance a bot should hold for an asset
pub fn get_min_balance(asset: &str) -> Decimal {
    max_deposit(asset) / dec!(10)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::dec;

    // --- max_deposit ---

    #[test]
    fn test_max_deposit_usdt() {
        assert_eq!(max_deposit("USDT"), dec!(1000));
    }

    #[test]
    fn test_max_deposit_btc() {
        assert_eq!(max_deposit("BTC"), dec!(0.05));
    }

    #[test]
    fn test_max_deposit_eth() {
        assert_eq!(max_deposit("ETH"), dec!(0.5));
    }

    #[test]
    fn test_max_deposit_sol() {
        assert_eq!(max_deposit("SOL"), dec!(5));
    }

    #[test]
    fn test_max_deposit_unknown_returns_zero() {
        assert_eq!(max_deposit("DOGE"), dec!(0));
    }

    #[test]
    fn test_max_deposit_empty_string_returns_zero() {
        assert_eq!(max_deposit(""), dec!(0));
    }

    #[test]
    fn test_max_deposit_case_sensitive() {
        // lowercase "usdt" is not a known asset
        assert_eq!(max_deposit("usdt"), dec!(0));
        assert_eq!(max_deposit("btc"), dec!(0));
    }

    // --- get_min_balance ---

    #[test]
    fn test_min_balance_usdt() {
        assert_eq!(get_min_balance("USDT"), dec!(100));
    }

    #[test]
    fn test_min_balance_btc() {
        assert_eq!(get_min_balance("BTC"), dec!(0.005));
    }

    #[test]
    fn test_min_balance_eth() {
        assert_eq!(get_min_balance("ETH"), dec!(0.05));
    }

    #[test]
    fn test_min_balance_sol() {
        assert_eq!(get_min_balance("SOL"), dec!(0.5));
    }

    #[test]
    fn test_min_balance_unknown_returns_zero() {
        assert_eq!(get_min_balance("XRP"), dec!(0));
    }

    #[test]
    fn test_min_balance_is_tenth_of_max_deposit() {
        // Invariant: min_balance == max_deposit / 10 for all known assets
        for asset in ["USDT", "BTC", "ETH", "SOL"] {
            let expected = max_deposit(asset) / dec!(10);
            assert_eq!(
                get_min_balance(asset),
                expected,
                "Failed invariant for asset: {asset}"
            );
        }
    }
}
