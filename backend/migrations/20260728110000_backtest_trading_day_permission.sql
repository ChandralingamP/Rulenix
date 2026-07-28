ALTER TABLE users
    ADD COLUMN IF NOT EXISTS can_backtest_on_trading_days BOOLEAN NOT NULL DEFAULT FALSE;

ALTER TABLE users
    ADD CONSTRAINT users_trading_day_backtest_requires_backtest
    CHECK (NOT can_backtest_on_trading_days OR can_backtest);

COMMENT ON COLUMN users.can_backtest_on_trading_days IS
    'May run backtests during an Indian trading day; requires can_backtest.';
