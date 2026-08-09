CREATE TABLE IF NOT EXISTS backtest_option_contracts (
    id UUID PRIMARY KEY,
    snapshot_date DATE NOT NULL,
    instrument VARCHAR(32) NOT NULL,
    side VARCHAR(2) NOT NULL CHECK (side IN ('CE', 'PE')),
    exchange VARCHAR(16) NOT NULL,
    symbol_token VARCHAR(32) NOT NULL,
    trading_symbol VARCHAR(96) NOT NULL,
    expiry_date DATE NOT NULL,
    strike_price DOUBLE PRECISION NOT NULL,
    lot_size INTEGER NOT NULL CHECK (lot_size > 0),
    source VARCHAR(32) NOT NULL DEFAULT 'angel_one_master',
    fetched_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (snapshot_date, instrument, side, symbol_token)
);

CREATE INDEX IF NOT EXISTS backtest_option_contracts_lookup_idx
    ON backtest_option_contracts (instrument, side, expiry_date, strike_price);

CREATE INDEX IF NOT EXISTS backtest_option_contracts_snapshot_idx
    ON backtest_option_contracts (instrument, snapshot_date DESC);

COMMENT ON TABLE backtest_option_contracts IS
    'Daily option contract snapshots retained for historically valid option-entry backtests.';
