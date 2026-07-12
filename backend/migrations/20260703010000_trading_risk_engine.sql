CREATE TABLE IF NOT EXISTS risk_limits (
    user_id UUID REFERENCES users(id) ON DELETE CASCADE,
    max_lots INTEGER CHECK (max_lots IS NULL OR max_lots > 0),
    max_quantity INTEGER CHECK (max_quantity IS NULL OR max_quantity > 0),
    max_notional NUMERIC(20,2) CHECK (max_notional IS NULL OR max_notional > 0),
    max_open_positions INTEGER CHECK (max_open_positions IS NULL OR max_open_positions > 0),
    max_trades_per_day INTEGER CHECK (max_trades_per_day IS NULL OR max_trades_per_day > 0),
    max_daily_realized_loss NUMERIC(20,2) CHECK (max_daily_realized_loss IS NULL OR max_daily_realized_loss > 0),
    max_daily_unrealized_loss NUMERIC(20,2) CHECK (max_daily_unrealized_loss IS NULL OR max_daily_unrealized_loss > 0),
    max_price_age_seconds INTEGER CHECK (max_price_age_seconds IS NULL OR max_price_age_seconds BETWEEN 1 AND 3600),
    margin_requirement_percent NUMERIC(5,2) CHECK (margin_requirement_percent IS NULL OR margin_requirement_percent BETWEEN 0 AND 100),
    updated_by UUID REFERENCES users(id) ON DELETE SET NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE UNIQUE INDEX IF NOT EXISTS risk_limits_global_idx ON risk_limits ((TRUE)) WHERE user_id IS NULL;
CREATE UNIQUE INDEX IF NOT EXISTS risk_limits_user_idx ON risk_limits (user_id) WHERE user_id IS NOT NULL;

INSERT INTO risk_limits (
    user_id,max_lots,max_quantity,max_notional,max_open_positions,max_trades_per_day,
    max_daily_realized_loss,max_daily_unrealized_loss,max_price_age_seconds,margin_requirement_percent
) SELECT NULL,20,10000,100000000,20,100,1000000,1000000,30,10
WHERE NOT EXISTS (SELECT 1 FROM risk_limits WHERE user_id IS NULL);

CREATE TABLE IF NOT EXISTS risk_kill_switches (
    user_id UUID REFERENCES users(id) ON DELETE CASCADE,
    enabled BOOLEAN NOT NULL DEFAULT FALSE,
    reason TEXT NOT NULL DEFAULT '',
    updated_by UUID REFERENCES users(id) ON DELETE SET NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE UNIQUE INDEX IF NOT EXISTS risk_kill_global_idx ON risk_kill_switches ((TRUE)) WHERE user_id IS NULL;
CREATE UNIQUE INDEX IF NOT EXISTS risk_kill_user_idx ON risk_kill_switches (user_id) WHERE user_id IS NOT NULL;
INSERT INTO risk_kill_switches (user_id,enabled,reason)
SELECT NULL,FALSE,'' WHERE NOT EXISTS (SELECT 1 FROM risk_kill_switches WHERE user_id IS NULL);

CREATE TABLE IF NOT EXISTS market_price_ticks (
    contract_token VARCHAR(32) PRIMARY KEY,
    price DOUBLE PRECISION NOT NULL CHECK (price > 0),
    received_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS broker_reconciliation_health (
    user_id UUID PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    healthy BOOLEAN NOT NULL,
    detail TEXT NOT NULL DEFAULT '',
    checked_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS risk_decisions (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    order_id UUID,
    execution_mode VARCHAR(8) NOT NULL CHECK (execution_mode IN ('demo','live')),
    order_role VARCHAR(16) NOT NULL,
    allowed BOOLEAN NOT NULL,
    reason_code VARCHAR(64) NOT NULL,
    message TEXT NOT NULL,
    values JSONB NOT NULL DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS risk_decisions_user_created_idx ON risk_decisions (user_id,created_at DESC);

ALTER TABLE strategy_orders ADD COLUMN IF NOT EXISTS risk_decision_id UUID REFERENCES risk_decisions(id);
