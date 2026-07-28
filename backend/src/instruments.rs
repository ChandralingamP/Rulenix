pub const FUTURES_BREAKOUT_INSTRUMENTS: [&str; 5] =
    ["GOLDTEN", "GOLDM", "SILVERM", "SILVERMIC", "NATGASMINI"];

pub fn is_futures_breakout_instrument(instrument: &str) -> bool {
    FUTURES_BREAKOUT_INSTRUMENTS.contains(&instrument)
}

pub fn futures_breakout_label(instrument: &str) -> &'static str {
    match instrument {
        "GOLDM" => "Gold Mini",
        "GOLDTEN" => "Gold Ten",
        "SILVERM" => "Silver Mini",
        "SILVERMIC" => "Silver Micro",
        "NATGASMINI" => "Natural Gas Mini",
        _ => "MCX Futures",
    }
}

pub fn futures_pnl_multiplier_per_lot(instrument: &str) -> f64 {
    match instrument {
        "GOLDM" => 10.0,
        "GOLDTEN" => 1.0,
        "SILVERM" => 5.0,
        "SILVERMIC" => 1.0,
        "NATGASMINI" => 250.0,
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
    fn supported_contracts_use_exchange_point_values() {
        assert_eq!(
            FUTURES_BREAKOUT_INSTRUMENTS,
            ["GOLDTEN", "GOLDM", "SILVERM", "SILVERMIC", "NATGASMINI"]
        );
        assert_eq!(futures_pnl_multiplier_per_lot("GOLDM"), 10.0);
        assert_eq!(futures_pnl_multiplier_per_lot("GOLDTEN"), 1.0);
        assert_eq!(futures_pnl_multiplier_per_lot("SILVERM"), 5.0);
        assert_eq!(futures_pnl_multiplier_per_lot("SILVERMIC"), 1.0);
        assert_eq!(futures_pnl_multiplier_per_lot("NATGASMINI"), 250.0);
    }

    #[test]
    fn broker_quantities_convert_to_contract_pnl_units() {
        assert_eq!(futures_pnl_units("GOLDM", 400, Some(100)), 40.0);
        assert_eq!(futures_pnl_units("GOLDTEN", 40, Some(10)), 4.0);
        assert_eq!(futures_pnl_units("SILVERM", 20, Some(5)), 20.0);
        assert_eq!(futures_pnl_units("SILVERMIC", 4, Some(1)), 4.0);
        assert_eq!(futures_pnl_units("NATGASMINI", 1_000, Some(250)), 1_000.0);
        assert_eq!(futures_pnl_units("OTHER", 40, Some(10)), 40.0);
    }

    #[test]
    fn full_size_gold_is_not_supported() {
        assert!(!is_futures_breakout_instrument("GOLD"));
        assert_eq!(futures_pnl_units("GOLD", 4, Some(1)), 4.0);
    }
}
