pub const FUTURES_BREAKOUT_INSTRUMENTS: [&str; 3] = ["GOLDTEN", "GOLDM", "GOLD"];

pub fn is_futures_breakout_instrument(instrument: &str) -> bool {
    FUTURES_BREAKOUT_INSTRUMENTS.contains(&instrument)
}

pub fn futures_breakout_label(instrument: &str) -> &'static str {
    match instrument {
        "GOLD" => "Gold",
        "GOLDM" => "Gold Mini",
        "GOLDTEN" => "Gold Ten",
        _ => "MCX Futures",
    }
}

pub fn futures_pnl_multiplier_per_lot(instrument: &str) -> f64 {
    match instrument {
        "GOLD" => 100.0,
        "GOLDM" => 10.0,
        "GOLDTEN" => 1.0,
        _ => 1.0,
    }
}

pub fn futures_pnl_units(instrument: &str, quantity: i32, lot_size: Option<i32>) -> f64 {
    let quantity = quantity.max(0) as f64;
    if !is_futures_breakout_instrument(instrument) {
        return quantity;
    }
    let lots = quantity / lot_size.unwrap_or(1).max(1) as f64;
    lots * futures_pnl_multiplier_per_lot(instrument)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_gold_contracts_use_exchange_point_values() {
        assert_eq!(futures_pnl_multiplier_per_lot("GOLD"), 100.0);
        assert_eq!(futures_pnl_multiplier_per_lot("GOLDM"), 10.0);
        assert_eq!(futures_pnl_multiplier_per_lot("GOLDTEN"), 1.0);
    }

    #[test]
    fn broker_quantities_convert_to_contract_pnl_units() {
        assert_eq!(futures_pnl_units("GOLD", 4, Some(1)), 400.0);
        assert_eq!(futures_pnl_units("GOLDM", 400, Some(100)), 40.0);
        assert_eq!(futures_pnl_units("GOLDTEN", 40, Some(10)), 4.0);
        assert_eq!(futures_pnl_units("OTHER", 40, Some(10)), 40.0);
    }
}
