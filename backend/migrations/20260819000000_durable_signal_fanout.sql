CREATE TABLE IF NOT EXISTS strategy_signals (
    id UUID PRIMARY KEY,
    strategy_key VARCHAR(64) NOT NULL,
    instrument VARCHAR(32) NOT NULL,
    session_key VARCHAR(96) NOT NULL,
    signal_at TIMESTAMPTZ NOT NULL,
    snapshot_id UUID REFERENCES strategy_market_snapshots(id) ON DELETE SET NULL,
    signal_type VARCHAR(32) NOT NULL,
    status VARCHAR(24) NOT NULL DEFAULT 'confirmed'
        CHECK (status IN ('confirmed', 'dispatching', 'completed', 'partial', 'failed', 'expired')),
    expected_users INTEGER NOT NULL DEFAULT 0 CHECK (expected_users >= 0),
    payload JSONB NOT NULL DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (strategy_key, instrument, session_key, signal_type)
);

CREATE INDEX IF NOT EXISTS strategy_signals_day_idx
    ON strategy_signals (signal_at DESC, strategy_key, instrument);

CREATE TABLE IF NOT EXISTS strategy_execution_intents (
    id UUID PRIMARY KEY,
    signal_id UUID NOT NULL REFERENCES strategy_signals(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    snapshot_id UUID REFERENCES strategy_market_snapshots(id) ON DELETE SET NULL,
    trade_id UUID REFERENCES trades(id) ON DELETE SET NULL,
    strategy_key VARCHAR(64) NOT NULL,
    instrument VARCHAR(32) NOT NULL,
    session_key VARCHAR(96) NOT NULL,
    action VARCHAR(32) NOT NULL,
    role VARCHAR(16) NOT NULL,
    side VARCHAR(4) NOT NULL CHECK (side IN ('BUY', 'SELL')),
    order_type VARCHAR(24) NOT NULL,
    lots INTEGER NOT NULL CHECK (lots > 0),
    quantity INTEGER,
    price DOUBLE PRECISION NOT NULL,
    trigger_price DOUBLE PRECISION,
    status VARCHAR(24) NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'claimed', 'retry_wait', 'submitted', 'completed', 'skipped', 'failed', 'expired')),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at TIMESTAMPTZ,
    last_error TEXT NOT NULL DEFAULT '',
    strategy_order_id UUID REFERENCES strategy_orders(id) ON DELETE SET NULL,
    claimed_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE UNIQUE INDEX IF NOT EXISTS strategy_entry_intents_signal_user_role_idx
    ON strategy_execution_intents (signal_id, user_id, action, role)
    WHERE trade_id IS NULL;

CREATE INDEX IF NOT EXISTS strategy_execution_intents_due_idx
    ON strategy_execution_intents (status, next_attempt_at, created_at)
    WHERE status IN ('pending', 'retry_wait');

CREATE INDEX IF NOT EXISTS strategy_execution_intents_user_day_idx
    ON strategy_execution_intents (user_id, created_at DESC);

CREATE UNIQUE INDEX IF NOT EXISTS strategy_square_off_intents_trade_idx
    ON strategy_execution_intents (trade_id, action)
    WHERE trade_id IS NOT NULL AND action = 'SQUARE_OFF';
