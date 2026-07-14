ALTER TABLE broker_margin_estimates
    ADD COLUMN IF NOT EXISTS order_type VARCHAR(32) NOT NULL DEFAULT 'STOPLOSS_LIMIT';

UPDATE broker_margin_estimates
SET order_type = COALESCE(NULLIF(raw_response->>'order_type', ''), order_type);

ALTER TABLE broker_margin_estimates
    DROP CONSTRAINT IF EXISTS broker_margin_estimates_exchange_symbol_token_product_type_trade_t_key;

CREATE UNIQUE INDEX IF NOT EXISTS broker_margin_estimates_unique_order_type_idx
    ON broker_margin_estimates (exchange, symbol_token, product_type, order_type, trade_type, lot_size);

DROP INDEX IF EXISTS broker_margin_estimates_lookup_idx;
CREATE INDEX IF NOT EXISTS broker_margin_estimates_lookup_idx
    ON broker_margin_estimates (exchange, symbol_token, product_type, order_type, trade_type, lot_size, fetched_at DESC);
