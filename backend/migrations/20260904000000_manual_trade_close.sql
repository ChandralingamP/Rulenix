CREATE TABLE IF NOT EXISTS manual_trade_close_intents (
    trade_id UUID PRIMARY KEY REFERENCES trades(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    status VARCHAR(32) NOT NULL DEFAULT 'requested'
        CHECK (status IN (
            'requested',
            'cancelling_protection',
            'submitted',
            'partially_filled',
            'ambiguous',
            'completed',
            'failed',
            'reconciliation_required'
        )),
    requested_quantity INTEGER NOT NULL CHECK (requested_quantity > 0),
    close_side VARCHAR(4) NOT NULL CHECK (close_side IN ('BUY', 'SELL')),
    strategy_order_id UUID REFERENCES strategy_orders(id) ON DELETE SET NULL,
    last_error TEXT NOT NULL DEFAULT '',
    requested_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    completed_at TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS manual_trade_close_intents_status_idx
    ON manual_trade_close_intents (status, updated_at)
    WHERE status <> 'completed';
