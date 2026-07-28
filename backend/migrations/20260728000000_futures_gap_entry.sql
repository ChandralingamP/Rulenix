ALTER TABLE strategy_market_snapshots
    ADD COLUMN IF NOT EXISTS previous_close DOUBLE PRECISION,
    ADD COLUMN IF NOT EXISTS market_open DOUBLE PRECISION,
    ADD COLUMN IF NOT EXISTS gap_direction VARCHAR(8),
    ADD COLUMN IF NOT EXISTS entry_direction VARCHAR(8),
    ADD COLUMN IF NOT EXISTS entry_source VARCHAR(24),
    ADD COLUMN IF NOT EXISTS gap_plan_status VARCHAR(24),
    ADD COLUMN IF NOT EXISTS opening_range_high DOUBLE PRECISION,
    ADD COLUMN IF NOT EXISTS opening_range_low DOUBLE PRECISION,
    ADD COLUMN IF NOT EXISTS planned_entry DOUBLE PRECISION,
    ADD COLUMN IF NOT EXISTS planned_target DOUBLE PRECISION,
    ADD COLUMN IF NOT EXISTS planned_sl1 DOUBLE PRECISION,
    ADD COLUMN IF NOT EXISTS planned_sl2 DOUBLE PRECISION,
    ADD COLUMN IF NOT EXISTS gap_planned_at TIMESTAMPTZ;

CREATE INDEX IF NOT EXISTS strategy_snapshots_gap_plan_idx
    ON strategy_market_snapshots (strategy_key, trade_date, gap_plan_status);
