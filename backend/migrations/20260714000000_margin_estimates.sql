ALTER TABLE backtest_runs DROP CONSTRAINT IF EXISTS backtest_runs_lookback_months_check;
ALTER TABLE backtest_runs
    ADD CONSTRAINT backtest_runs_lookback_months_check
    CHECK (lookback_months IN (1, 3, 6));

CREATE TABLE IF NOT EXISTS broker_margin_estimates (
    id UUID PRIMARY KEY,
    exchange VARCHAR(16) NOT NULL,
    symbol_token VARCHAR(32) NOT NULL,
    trading_symbol VARCHAR(96) NOT NULL,
    product_type VARCHAR(32) NOT NULL,
    trade_type VARCHAR(8) NOT NULL,
    lot_size INTEGER NOT NULL CHECK (lot_size > 0),
    margin_per_lot DOUBLE PRECISION NOT NULL CHECK (margin_per_lot >= 0),
    source VARCHAR(32) NOT NULL DEFAULT 'angel_margin_calculator',
    raw_response JSONB NOT NULL DEFAULT '{}',
    fetched_by UUID REFERENCES users(id) ON DELETE SET NULL,
    fetched_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (exchange, symbol_token, product_type, trade_type, lot_size)
);

CREATE INDEX IF NOT EXISTS broker_margin_estimates_lookup_idx
    ON broker_margin_estimates (exchange, symbol_token, product_type, trade_type, lot_size, fetched_at DESC);

ALTER TABLE strategy_orders
    ADD COLUMN IF NOT EXISTS margin_required DOUBLE PRECISION NOT NULL DEFAULT 0;

ALTER TABLE trades
    ADD COLUMN IF NOT EXISTS margin_required DOUBLE PRECISION NOT NULL DEFAULT 0;
