ALTER TABLE users ADD COLUMN IF NOT EXISTS can_backtest BOOLEAN NOT NULL DEFAULT FALSE;

CREATE INDEX IF NOT EXISTS users_can_backtest_idx ON users (can_backtest) WHERE can_backtest = TRUE;

COMMENT ON COLUMN users.can_backtest IS 'May run historical strategy backtests and view backtesting pages.';

CREATE TABLE IF NOT EXISTS backtest_market_candles (
    id UUID PRIMARY KEY,
    exchange VARCHAR(16) NOT NULL,
    instrument VARCHAR(32) NOT NULL,
    symbol_token VARCHAR(32) NOT NULL,
    trading_symbol VARCHAR(96) NOT NULL,
    interval_key VARCHAR(32) NOT NULL,
    candle_time TIMESTAMPTZ NOT NULL,
    open_price DOUBLE PRECISION NOT NULL,
    high_price DOUBLE PRECISION NOT NULL,
    low_price DOUBLE PRECISION NOT NULL,
    close_price DOUBLE PRECISION NOT NULL,
    volume DOUBLE PRECISION NOT NULL DEFAULT 0,
    source VARCHAR(32) NOT NULL DEFAULT 'angel_one',
    fetched_by UUID REFERENCES users(id) ON DELETE SET NULL,
    fetched_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (exchange, symbol_token, interval_key, candle_time)
);

CREATE INDEX IF NOT EXISTS backtest_market_candles_lookup_idx
    ON backtest_market_candles (instrument, interval_key, candle_time);

CREATE TABLE IF NOT EXISTS backtest_runs (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    strategy_key VARCHAR(64) NOT NULL,
    instrument VARCHAR(32) NOT NULL,
    trading_symbol VARCHAR(96) NOT NULL,
    symbol_token VARCHAR(32) NOT NULL,
    interval_key VARCHAR(32) NOT NULL,
    lookback_months INTEGER NOT NULL CHECK (lookback_months IN (3, 6)),
    from_time TIMESTAMPTZ NOT NULL,
    to_time TIMESTAMPTZ NOT NULL,
    lots INTEGER NOT NULL CHECK (lots > 0),
    lot_size INTEGER NOT NULL CHECK (lot_size > 0),
    status VARCHAR(16) NOT NULL CHECK (status IN ('completed', 'failed')),
    summary JSONB NOT NULL DEFAULT '{}',
    error TEXT,
    data_points INTEGER NOT NULL DEFAULT 0,
    reused_points INTEGER NOT NULL DEFAULT 0,
    fetched_points INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS backtest_runs_user_idx
    ON backtest_runs (user_id, created_at DESC);
CREATE INDEX IF NOT EXISTS backtest_runs_reuse_idx
    ON backtest_runs (strategy_key, instrument, interval_key, lookback_months, created_at DESC);

CREATE TABLE IF NOT EXISTS backtest_trades (
    id UUID PRIMARY KEY,
    run_id UUID NOT NULL REFERENCES backtest_runs(id) ON DELETE CASCADE,
    trade_date DATE NOT NULL,
    direction VARCHAR(4) NOT NULL CHECK (direction IN ('BUY', 'SELL')),
    entry_time TIMESTAMPTZ NOT NULL,
    entry_price DOUBLE PRECISION NOT NULL,
    exit_time TIMESTAMPTZ NOT NULL,
    exit_price DOUBLE PRECISION NOT NULL,
    lots INTEGER NOT NULL,
    quantity INTEGER NOT NULL,
    realized_pnl DOUBLE PRECISION NOT NULL,
    exit_reason VARCHAR(32) NOT NULL,
    levels JSONB NOT NULL DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS backtest_trades_run_idx
    ON backtest_trades (run_id, entry_time);
