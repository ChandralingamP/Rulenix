ALTER TABLE strategy_orders ADD COLUMN IF NOT EXISTS client_order_id VARCHAR(32) NOT NULL DEFAULT '';
ALTER TABLE strategy_orders ADD COLUMN IF NOT EXISTS broker_error_class VARCHAR(24) NOT NULL DEFAULT '';
ALTER TABLE strategy_orders ADD COLUMN IF NOT EXISTS broker_error_code VARCHAR(64) NOT NULL DEFAULT '';
ALTER TABLE strategy_orders ADD COLUMN IF NOT EXISTS submission_attempts INTEGER NOT NULL DEFAULT 0;
ALTER TABLE strategy_orders ADD COLUMN IF NOT EXISTS filled_quantity INTEGER NOT NULL DEFAULT 0;
ALTER TABLE strategy_orders ADD COLUMN IF NOT EXISTS processed_quantity INTEGER NOT NULL DEFAULT 0;
ALTER TABLE strategy_orders ADD COLUMN IF NOT EXISTS average_fill_price DOUBLE PRECISION;
ALTER TABLE strategy_orders ADD COLUMN IF NOT EXISTS last_reconciled_at TIMESTAMPTZ;
ALTER TABLE strategy_orders ADD COLUMN IF NOT EXISTS state_version BIGINT NOT NULL DEFAULT 0;

UPDATE strategy_orders SET client_order_id='RX'||UPPER(SUBSTRING(REPLACE(id::text,'-',''),1,18))
WHERE client_order_id='';
CREATE UNIQUE INDEX IF NOT EXISTS strategy_orders_client_order_id_idx
    ON strategy_orders(client_order_id) WHERE client_order_id<>'';
CREATE INDEX IF NOT EXISTS strategy_orders_live_state_idx
    ON strategy_orders(user_id,status,updated_at)
    WHERE execution_mode='live' AND status IN ('submitting','ambiguous','submitted','partially_filled','processing','cancelling');

CREATE TABLE IF NOT EXISTS broker_order_events (
    id BIGSERIAL PRIMARY KEY,
    order_id UUID NOT NULL REFERENCES strategy_orders(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    from_state VARCHAR(24) NOT NULL,
    to_state VARCHAR(24) NOT NULL,
    event_type VARCHAR(48) NOT NULL,
    broker_order_id VARCHAR(96) NOT NULL DEFAULT '',
    error_class VARCHAR(24) NOT NULL DEFAULT '',
    error_code VARCHAR(64) NOT NULL DEFAULT '',
    diagnostic TEXT NOT NULL DEFAULT '',
    broker_payload JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS broker_order_events_order_idx ON broker_order_events(order_id,id);
