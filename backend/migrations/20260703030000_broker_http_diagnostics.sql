ALTER TABLE strategy_orders ADD COLUMN IF NOT EXISTS broker_http_status INTEGER;
ALTER TABLE broker_order_events ADD COLUMN IF NOT EXISTS http_status INTEGER;
