WITH target_fills AS (
    SELECT DISTINCT ON (trade_id)
        trade_id,
        COALESCE(average_fill_price, price)::numeric(18,2) AS price,
        COALESCE(filled_at, updated_at) AS filled_at,
        processed_quantity
    FROM strategy_orders
    WHERE trade_id IS NOT NULL
      AND role = 'TARGET'
      AND processed_quantity > 0
    ORDER BY trade_id, COALESCE(filled_at, updated_at) DESC, id DESC
)
UPDATE trades AS trade
SET tp1_exit_price = target.price,
    tp1_exit_datetime = target.filled_at,
    tp1_exit_quantity = target.processed_quantity
FROM target_fills AS target
WHERE trade.id = target.trade_id;

WITH final_fills AS (
    SELECT DISTINCT ON (trade_id)
        trade_id,
        role,
        session_key
    FROM strategy_orders
    WHERE trade_id IS NOT NULL
      AND role IN ('TARGET', 'SL1', 'SL2')
      AND processed_quantity > 0
    ORDER BY trade_id, COALESCE(filled_at, updated_at) DESC, id DESC
)
UPDATE trades AS trade
SET exit_reason = CASE
        WHEN final.session_key LIKE 'optsq-%' OR final.session_key LIKE 'stsq-%'
            THEN 'MARKET_CLOSED'
        WHEN final.session_key LIKE 'strev-%'
            THEN 'SIGNAL_REVERSAL'
        WHEN trade.strategy_key = 'futures_breakout_v3' AND final.role = 'TARGET'
            THEN 'TP1'
        WHEN trade.strategy_key = 'futures_breakout_v3'
            THEN final.role
        WHEN final.role = 'TARGET' THEN 'TP'
        ELSE 'SL'
    END
FROM final_fills AS final
WHERE trade.id = final.trade_id
  AND trade.status = 'closed';
