ALTER TABLE strategy_market_snapshots
    ADD COLUMN IF NOT EXISTS exchange_segment VARCHAR(8) NOT NULL DEFAULT 'MCX',
    ADD COLUMN IF NOT EXISTS product_type VARCHAR(24) NOT NULL DEFAULT 'CARRYFORWARD',
    ADD COLUMN IF NOT EXISTS execution_key VARCHAR(96) NOT NULL DEFAULT 'daily',
    ADD COLUMN IF NOT EXISTS underlying_token VARCHAR(32) NOT NULL DEFAULT '';

ALTER TABLE strategy_market_snapshots
    DROP CONSTRAINT IF EXISTS strategy_market_snapshots_strategy_key_instrument_trade_date_key;

CREATE UNIQUE INDEX IF NOT EXISTS strategy_market_snapshots_execution_idx
    ON strategy_market_snapshots (strategy_key, instrument, trade_date, execution_key);

ALTER TABLE strategy_orders
    ADD COLUMN IF NOT EXISTS order_type VARCHAR(24) NOT NULL DEFAULT 'LIMIT',
    ADD COLUMN IF NOT EXISTS exchange_segment VARCHAR(8) NOT NULL DEFAULT 'MCX',
    ADD COLUMN IF NOT EXISTS product_type VARCHAR(24) NOT NULL DEFAULT 'CARRYFORWARD';

UPDATE strategy_orders
SET order_type = CASE
        WHEN trigger_price IS NOT NULL THEN 'STOPLOSS_LIMIT'
        ELSE 'LIMIT'
    END
WHERE order_type = 'LIMIT';

ALTER TABLE trades
    ADD COLUMN IF NOT EXISTS signal_direction VARCHAR(4),
    ADD COLUMN IF NOT EXISTS option_type VARCHAR(4),
    ADD COLUMN IF NOT EXISTS underlying_token VARCHAR(32) NOT NULL DEFAULT '';

ALTER TABLE user_strategy_configs
    ADD COLUMN IF NOT EXISTS interval_key VARCHAR(32) NOT NULL DEFAULT 'FIVE_MINUTE',
    ADD COLUMN IF NOT EXISTS stop_loss_percent DOUBLE PRECISION NOT NULL DEFAULT 5.0,
    ADD COLUMN IF NOT EXISTS target_percent DOUBLE PRECISION NOT NULL DEFAULT 20.0,
    ADD COLUMN IF NOT EXISTS keltner_multiplier DOUBLE PRECISION NOT NULL DEFAULT 2.0,
    ADD COLUMN IF NOT EXISTS require_volume BOOLEAN NOT NULL DEFAULT FALSE,
    ADD COLUMN IF NOT EXISTS premium_min DOUBLE PRECISION NOT NULL DEFAULT 200.0,
    ADD COLUMN IF NOT EXISTS premium_max DOUBLE PRECISION NOT NULL DEFAULT 300.0;

CREATE TABLE IF NOT EXISTS ichimoku_signal_evaluations (
    id UUID PRIMARY KEY,
    instrument VARCHAR(32) NOT NULL,
    interval_key VARCHAR(32) NOT NULL,
    variant_key VARCHAR(192) NOT NULL,
    candle_time TIMESTAMPTZ NOT NULL,
    signal_direction VARCHAR(4),
    indicator_exit_buy BOOLEAN NOT NULL DEFAULT FALSE,
    indicator_exit_sell BOOLEAN NOT NULL DEFAULT FALSE,
    candle_count INTEGER NOT NULL DEFAULT 0,
    evaluated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (instrument, interval_key, variant_key, candle_time)
);

CREATE INDEX IF NOT EXISTS ichimoku_signal_evaluations_latest_idx
    ON ichimoku_signal_evaluations (instrument, interval_key, variant_key, candle_time DESC);

INSERT INTO user_strategy_activations (user_id, strategy_key, is_active)
SELECT id, 'ichimoku_keltner_tsi', FALSE
FROM users
ON CONFLICT (user_id, strategy_key) DO NOTHING;
