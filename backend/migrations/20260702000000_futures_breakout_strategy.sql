CREATE TABLE IF NOT EXISTS strategy_market_snapshots (
    id UUID PRIMARY KEY,
    strategy_key VARCHAR(64) NOT NULL,
    instrument VARCHAR(32) NOT NULL,
    trade_date DATE NOT NULL,
    status VARCHAR(16) NOT NULL CHECK (status IN ('ready', 'missing', 'failed')),
    error TEXT,
    contract_token VARCHAR(32),
    contract_symbol VARCHAR(96),
    contract_expiry DATE,
    lot_size INTEGER,
    candle_dates DATE[] NOT NULL DEFAULT '{}',
    highs DOUBLE PRECISION[] NOT NULL DEFAULT '{}',
    lows DOUBLE PRECISION[] NOT NULL DEFAULT '{}',
    hh2 DOUBLE PRECISION,
    ll2 DOUBLE PRECISION,
    hh4 DOUBLE PRECISION,
    ll4 DOUBLE PRECISION,
    buy_entry DOUBLE PRECISION,
    buy_target DOUBLE PRECISION,
    buy_sl1 DOUBLE PRECISION,
    buy_sl2 DOUBLE PRECISION,
    sell_entry DOUBLE PRECISION,
    sell_target DOUBLE PRECISION,
    sell_sl1 DOUBLE PRECISION,
    sell_sl2 DOUBLE PRECISION,
    fetched_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (strategy_key, instrument, trade_date)
);
CREATE INDEX IF NOT EXISTS strategy_snapshots_lookup_idx
    ON strategy_market_snapshots (instrument, trade_date DESC);

CREATE TABLE IF NOT EXISTS user_strategy_configs (
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    strategy_key VARCHAR(64) NOT NULL DEFAULT 'futures_breakout_v3',
    instrument VARCHAR(32) NOT NULL DEFAULT 'GOLDTEN',
    enabled BOOLEAN NOT NULL DEFAULT FALSE,
    lots INTEGER NOT NULL DEFAULT 1 CHECK (lots > 0),
    run_day_session BOOLEAN NOT NULL DEFAULT TRUE,
    run_evening_session BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (user_id, strategy_key, instrument)
);

CREATE TABLE IF NOT EXISTS strategy_orders (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    snapshot_id UUID NOT NULL REFERENCES strategy_market_snapshots(id),
    trade_id UUID REFERENCES trades(id) ON DELETE SET NULL,
    session_key VARCHAR(32) NOT NULL,
    role VARCHAR(16) NOT NULL CHECK (role IN ('BUY_ENTRY','SELL_ENTRY','TARGET','SL1','SL2')),
    side VARCHAR(4) NOT NULL CHECK (side IN ('BUY','SELL')),
    execution_mode VARCHAR(8) NOT NULL CHECK (execution_mode IN ('demo','live')),
    lots INTEGER NOT NULL CHECK (lots > 0),
    quantity INTEGER NOT NULL CHECK (quantity > 0),
    price DOUBLE PRECISION NOT NULL,
    trigger_price DOUBLE PRECISION,
    status VARCHAR(16) NOT NULL DEFAULT 'pending',
    broker_order_id VARCHAR(96) NOT NULL DEFAULT '',
    broker_status TEXT NOT NULL DEFAULT '',
    idempotency_key VARCHAR(192) NOT NULL UNIQUE,
    filled_price DOUBLE PRECISION,
    filled_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS strategy_orders_reconcile_idx
    ON strategy_orders (user_id, execution_mode, status, created_at);

CREATE TABLE IF NOT EXISTS strategy_events (
    id BIGSERIAL PRIMARY KEY,
    user_id UUID REFERENCES users(id) ON DELETE CASCADE,
    strategy_key VARCHAR(64) NOT NULL DEFAULT 'futures_breakout_v3',
    instrument VARCHAR(32) NOT NULL DEFAULT 'GOLDTEN',
    event_type VARCHAR(48) NOT NULL,
    payload JSONB NOT NULL DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS strategy_events_user_idx
    ON strategy_events (user_id, id DESC);

ALTER TABLE trades ADD COLUMN IF NOT EXISTS strategy_key VARCHAR(64) NOT NULL DEFAULT '';
ALTER TABLE trades ADD COLUMN IF NOT EXISTS strategy_snapshot_id UUID REFERENCES strategy_market_snapshots(id);
ALTER TABLE trades ADD COLUMN IF NOT EXISTS total_lots INTEGER NOT NULL DEFAULT 0;
ALTER TABLE trades ADD COLUMN IF NOT EXISTS remaining_lots INTEGER NOT NULL DEFAULT 0;
ALTER TABLE trades ADD COLUMN IF NOT EXISTS target_price DOUBLE PRECISION;
ALTER TABLE trades ADD COLUMN IF NOT EXISTS sl1_price DOUBLE PRECISION;
ALTER TABLE trades ADD COLUMN IF NOT EXISTS sl2_price DOUBLE PRECISION;
