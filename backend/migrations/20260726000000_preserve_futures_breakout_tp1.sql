WITH original_targets AS (
    SELECT DISTINCT ON (orders.trade_id)
        orders.trade_id,
        orders.price
    FROM strategy_orders orders
    JOIN trades trade ON trade.id = orders.trade_id
    WHERE trade.strategy_key = 'futures_breakout_v3'
      AND trade.status = 'open'
      AND orders.role = 'TARGET'
      AND orders.price > 0
    ORDER BY orders.trade_id, orders.created_at
)
UPDATE trades trade
SET target_price = original_targets.price,
    updated_at = NOW()
FROM original_targets
WHERE trade.id = original_targets.trade_id
  AND trade.strategy_key = 'futures_breakout_v3'
  AND trade.status = 'open';
