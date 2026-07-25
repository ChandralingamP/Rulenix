CREATE TABLE IF NOT EXISTS strategy_reversal_intents (
    source_trade_id UUID PRIMARY KEY REFERENCES trades(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    snapshot_id UUID NOT NULL REFERENCES strategy_market_snapshots(id),
    instrument VARCHAR(32) NOT NULL,
    source_direction VARCHAR(4) NOT NULL CHECK (source_direction IN ('BUY', 'SELL')),
    reversal_direction VARCHAR(4) NOT NULL CHECK (reversal_direction IN ('BUY', 'SELL')),
    lots INTEGER NOT NULL CHECK (lots > 0),
    entry_price DOUBLE PRECISION NOT NULL CHECK (entry_price > 0),
    order_session_key VARCHAR(32) NOT NULL UNIQUE,
    status VARCHAR(24) NOT NULL DEFAULT 'pending'
        CHECK (status IN (
            'pending',
            'processing',
            'waiting',
            'submitted',
            'completed',
            'failed',
            'cancelled'
        )),
    attempts INTEGER NOT NULL DEFAULT 0,
    next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_error TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS strategy_reversal_intents_due_idx
    ON strategy_reversal_intents (status, next_attempt_at);
