use rand::RngExt;
use rust_decimal::{Decimal, dec};

// get random number within the min and max range
pub fn random_number(min: usize, max: usize) -> usize {
    rand::rng().random_range(min..=max)
}

// get random decimal within the min and max range
pub fn random_decimal(min: Decimal, max: Decimal) -> Decimal {
    let scale = 1_000_000; // precision
    let r = rand::rng().random_range(0..=scale);

    let fraction = Decimal::from(r) / Decimal::from(scale);

    min + (max - min) * fraction
}

// Amount of base quantity that can be bought with available quote balance
pub fn random_base_quantity_to_buy(
    quote_balance: Decimal,
    price: Decimal,
    min: Decimal,
    max: Decimal,
) -> Decimal {
    if price <= Decimal::ZERO {
        return Decimal::ZERO;
    }

    // Max you can spend
    let max_notional = quote_balance.min(dec!(1000));

    if max_notional <= Decimal::ZERO {
        return Decimal::ZERO;
    }

    // Sample notional (USD)
    let random_num = random_decimal(min, max);
    let notional = random_num * max_notional;

    // Convert to base quantity
    notional / price
}

// Amount of base quantity that can be sold in respect to max quote that can be spent
pub fn random_base_quantity_to_sell(
    base_balance: Decimal,
    price: Decimal,
    min: Decimal,
    max: Decimal,
) -> Decimal {
    if price <= Decimal::ZERO {
        return Decimal::ZERO;
    }

    // Convert base balance -> notional (USD)
    let base_notional = base_balance * price;

    // Cap by max allowed notional
    let max_notional = base_notional.min(dec!(1000));

    if max_notional <= Decimal::ZERO {
        return Decimal::ZERO;
    }

    // Sample notional (USD)
    let random_num = random_decimal(min, max);
    let notional = random_num * max_notional;

    // Convert back to base quantity
    notional / price
}

// compute random quote that can be spent
pub fn random_quote_to_spend(quote_balance: Decimal, min: Decimal, max: Decimal) -> Decimal {
    let max_notional = quote_balance.min(dec!(1000));

    if max_notional <= Decimal::ZERO {
        return Decimal::ZERO;
    }

    let random_num = random_decimal(min, max);
    random_num * max_notional // return USDT to spend
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::dec;

    const ITERATIONS: usize = 200;

    // --- random_number ---

    #[test]
    fn test_random_number_within_range() {
        for _ in 0..ITERATIONS {
            let result = random_number(5, 15);
            assert!(result >= 5 && result <= 15);
        }
    }

    #[test]
    fn test_random_number_single_value_range() {
        for _ in 0..ITERATIONS {
            assert_eq!(random_number(42, 42), 42);
        }
    }

    #[test]
    fn test_random_number_zero_range() {
        for _ in 0..ITERATIONS {
            assert_eq!(random_number(0, 0), 0);
        }
    }

    // --- random_decimal ---

    #[test]
    fn test_random_decimal_within_range() {
        let (min, max) = (dec!(0.1), dec!(0.9));
        for _ in 0..ITERATIONS {
            let result = random_decimal(min, max);
            assert!(result >= min && result <= max, "got {result}");
        }
    }

    #[test]
    fn test_random_decimal_single_value_range() {
        let value = dec!(0.5);
        for _ in 0..ITERATIONS {
            let result = random_decimal(value, value);
            assert_eq!(result, value);
        }
    }

    #[test]
    fn test_random_decimal_zero_to_one_range() {
        for _ in 0..ITERATIONS {
            let result = random_decimal(dec!(0), dec!(1));
            assert!(result >= dec!(0) && result <= dec!(1), "got {result}");
        }
    }

    // --- random_base_quantity_to_buy ---

    #[test]
    fn test_buy_zero_price_returns_zero() {
        for _ in 0..ITERATIONS {
            let result = random_base_quantity_to_buy(dec!(500), dec!(0), dec!(0.1), dec!(0.9));
            assert_eq!(result, dec!(0));
        }
    }

    #[test]
    fn test_buy_negative_price_returns_zero() {
        for _ in 0..ITERATIONS {
            let result = random_base_quantity_to_buy(dec!(500), dec!(-1), dec!(0.1), dec!(0.9));
            assert_eq!(result, dec!(0));
        }
    }

    #[test]
    fn test_buy_zero_balance_returns_zero() {
        for _ in 0..ITERATIONS {
            let result = random_base_quantity_to_buy(dec!(0), dec!(100), dec!(0.1), dec!(0.9));
            assert_eq!(result, dec!(0));
        }
    }

    #[test]
    fn test_buy_negative_balance_returns_zero() {
        for _ in 0..ITERATIONS {
            let result = random_base_quantity_to_buy(dec!(-50), dec!(100), dec!(0.1), dec!(0.9));
            assert_eq!(result, dec!(0));
        }
    }

    #[test]
    fn test_buy_result_is_positive() {
        for _ in 0..ITERATIONS {
            let result = random_base_quantity_to_buy(dec!(500), dec!(50000), dec!(0.1), dec!(0.9));
            assert!(result > dec!(0), "expected positive quantity, got {result}");
        }
    }

    #[test]
    fn test_buy_notional_cap_at_1000() {
        // quote_balance >> 1000, so max_notional is capped at 1000.
        // With min=1.0, max=1.0 the notional is exactly 1000, so quantity = 1000 / price.
        let price = dec!(50000);
        let result = random_base_quantity_to_buy(dec!(999999), price, dec!(1), dec!(1));
        let expected = dec!(1000) / price;
        assert_eq!(result, expected);
    }

    #[test]
    fn test_buy_respects_notional_bounds() {
        // quantity * price must be in [min * effective_notional, max * effective_notional]
        let (quote_balance, price) = (dec!(500), dec!(100));
        let (min, max) = (dec!(0.1), dec!(0.9));
        let effective_notional = quote_balance.min(dec!(1000)); // 500

        for _ in 0..ITERATIONS {
            let qty = random_base_quantity_to_buy(quote_balance, price, min, max);
            let spent = qty * price;
            assert!(spent >= min * effective_notional, "spent {spent} below min");
            assert!(spent <= max * effective_notional, "spent {spent} above max");
        }
    }

    // --- random_base_quantity_to_sell ---

    #[test]
    fn test_sell_zero_price_returns_zero() {
        for _ in 0..ITERATIONS {
            let result = random_base_quantity_to_sell(dec!(1), dec!(0), dec!(0.1), dec!(0.9));
            assert_eq!(result, dec!(0));
        }
    }

    #[test]
    fn test_sell_negative_price_returns_zero() {
        for _ in 0..ITERATIONS {
            let result = random_base_quantity_to_sell(dec!(1), dec!(-100), dec!(0.1), dec!(0.9));
            assert_eq!(result, dec!(0));
        }
    }

    #[test]
    fn test_sell_zero_balance_returns_zero() {
        for _ in 0..ITERATIONS {
            let result = random_base_quantity_to_sell(dec!(0), dec!(100), dec!(0.1), dec!(0.9));
            assert_eq!(result, dec!(0));
        }
    }

    #[test]
    fn test_sell_result_is_positive() {
        for _ in 0..ITERATIONS {
            let result = random_base_quantity_to_sell(dec!(1), dec!(50000), dec!(0.1), dec!(0.9));
            assert!(result > dec!(0), "expected positive quantity, got {result}");
        }
    }

    #[test]
    fn test_sell_notional_cap_at_1000() {
        // base_notional = 1 BTC * 999_999 price >> 1000, so capped.
        // With min=max=1.0, notional = 1000 exactly, quantity = 1000 / price.
        let price = dec!(999999);
        let result = random_base_quantity_to_sell(dec!(1), price, dec!(1), dec!(1));
        let expected = dec!(1000) / price;
        assert_eq!(result, expected);
    }

    #[test]
    fn test_sell_respects_notional_bounds() {
        let (base_balance, price) = (dec!(0.01), dec!(50000));
        let (min, max) = (dec!(0.1), dec!(0.9));
        let base_notional = base_balance * price; // 500
        let effective_notional = base_notional.min(dec!(1000));

        for _ in 0..ITERATIONS {
            let qty = random_base_quantity_to_sell(base_balance, price, min, max);
            let value_sold = qty * price;
            assert!(
                value_sold >= min * effective_notional,
                "sold {value_sold} below min"
            );
            assert!(
                value_sold <= max * effective_notional,
                "sold {value_sold} above max"
            );
        }
    }

    // --- random_quote_to_spend ---

    #[test]
    fn test_quote_zero_balance_returns_zero() {
        for _ in 0..ITERATIONS {
            let result = random_quote_to_spend(dec!(0), dec!(0.1), dec!(0.9));
            assert_eq!(result, dec!(0));
        }
    }

    #[test]
    fn test_quote_negative_balance_returns_zero() {
        for _ in 0..ITERATIONS {
            let result = random_quote_to_spend(dec!(-100), dec!(0.1), dec!(0.9));
            assert_eq!(result, dec!(0));
        }
    }

    #[test]
    fn test_quote_result_is_positive() {
        for _ in 0..ITERATIONS {
            let result = random_quote_to_spend(dec!(500), dec!(0.1), dec!(0.9));
            assert!(result > dec!(0), "expected positive quote, got {result}");
        }
    }

    #[test]
    fn test_quote_notional_cap_at_1000() {
        // With min=max=1.0, result must equal exactly 1000 regardless of balance.
        let result = random_quote_to_spend(dec!(999999), dec!(1), dec!(1));
        assert_eq!(result, dec!(1000));
    }

    #[test]
    fn test_quote_respects_notional_bounds() {
        let quote_balance = dec!(800);
        let (min, max) = (dec!(0.1), dec!(0.9));
        let effective_notional = quote_balance.min(dec!(1000)); // 800

        for _ in 0..ITERATIONS {
            let result = random_quote_to_spend(quote_balance, min, max);
            assert!(
                result >= min * effective_notional,
                "result {result} below min"
            );
            assert!(
                result <= max * effective_notional,
                "result {result} above max"
            );
        }
    }

    #[test]
    fn test_quote_balance_below_cap_uses_full_balance() {
        // balance (200) < 1000 cap, so effective_notional = 200.
        // With min=max=0.5, result = 0.5 * 200 = 100 exactly.
        let result = random_quote_to_spend(dec!(200), dec!(0.5), dec!(0.5));
        assert_eq!(result, dec!(100));
    }
}
