ALTER TABLE strategy_market_snapshots
    DROP CONSTRAINT IF EXISTS strategy_market_snapshots_strategy_key_instrument_trade_dat_key;

CREATE UNIQUE INDEX IF NOT EXISTS strategy_market_snapshots_execution_idx
    ON strategy_market_snapshots (strategy_key, instrument, trade_date, execution_key);
