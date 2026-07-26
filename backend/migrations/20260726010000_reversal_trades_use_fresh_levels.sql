ALTER TABLE trades
    ADD COLUMN IF NOT EXISTS reversal_of_trade_id UUID
    REFERENCES trades(id) ON DELETE SET NULL;

CREATE INDEX IF NOT EXISTS trades_reversal_of_idx
    ON trades (reversal_of_trade_id)
    WHERE reversal_of_trade_id IS NOT NULL;

UPDATE trades AS reversal
SET reversal_of_trade_id = intent.source_trade_id,
    updated_at = NOW()
FROM strategy_orders AS entry_order
JOIN strategy_reversal_intents AS intent
  ON intent.user_id = entry_order.user_id
 AND intent.order_session_key = entry_order.session_key
WHERE entry_order.trade_id = reversal.id
  AND entry_order.role IN ('BUY_ENTRY', 'SELL_ENTRY')
  AND reversal.strategy_key = 'futures_breakout_v3'
  AND reversal.reversal_of_trade_id IS NULL;

UPDATE trades AS reversal
SET target_price = CASE
        WHEN EXISTS (
            SELECT 1
            FROM strategy_orders AS target_order
            WHERE target_order.trade_id = reversal.id
              AND target_order.role = 'TARGET'
              AND target_order.processed_quantity > 0
        ) THEN reversal.target_price
        WHEN reversal.direction = 'BUY' THEN reversal.entry_price::float8 * (1.0 + 0.015)
        ELSE reversal.entry_price::float8 * (1.0 - 0.015)
    END,
    sl1_price = CASE
        WHEN reversal.direction = 'BUY' THEN CASE
            WHEN snapshot.ll2 * (1.0 - 0.0012) < reversal.entry_price::float8
                THEN GREATEST(
                    reversal.entry_price::float8 * (1.0 - 0.015),
                    snapshot.ll2 * (1.0 - 0.0012)
                )
            ELSE reversal.entry_price::float8 * (1.0 - 0.015)
        END
        ELSE CASE
            WHEN snapshot.hh2 * (1.0 + 0.0012) > reversal.entry_price::float8
                THEN LEAST(
                    reversal.entry_price::float8 * (1.0 + 0.015),
                    snapshot.hh2 * (1.0 + 0.0012)
                )
            ELSE reversal.entry_price::float8 * (1.0 + 0.015)
        END
    END,
    sl2_price = CASE
        WHEN reversal.direction = 'BUY' THEN CASE
            WHEN snapshot.ll4 * (1.0 - 0.0012) < reversal.entry_price::float8
                THEN GREATEST(
                    reversal.entry_price::float8 * (1.0 - 0.015),
                    snapshot.ll4 * (1.0 - 0.0012)
                )
            ELSE reversal.entry_price::float8 * (1.0 - 0.015)
        END
        ELSE CASE
            WHEN snapshot.hh4 * (1.0 + 0.0012) > reversal.entry_price::float8
                THEN LEAST(
                    reversal.entry_price::float8 * (1.0 + 0.015),
                    snapshot.hh4 * (1.0 + 0.0012)
                )
            ELSE reversal.entry_price::float8 * (1.0 + 0.015)
        END
    END,
    updated_at = NOW()
FROM strategy_market_snapshots AS snapshot
WHERE reversal.strategy_snapshot_id = snapshot.id
  AND reversal.strategy_key = 'futures_breakout_v3'
  AND reversal.status = 'open'
  AND reversal.reversal_of_trade_id IS NOT NULL
  AND reversal.direction IN ('BUY', 'SELL')
  AND snapshot.hh2 IS NOT NULL
  AND snapshot.ll2 IS NOT NULL
  AND snapshot.hh4 IS NOT NULL
  AND snapshot.ll4 IS NOT NULL;
