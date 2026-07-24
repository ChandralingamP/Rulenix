CREATE TEMP TABLE goldten_demo_pnl_fix ON COMMIT DROP AS
WITH exit_fills AS (
    SELECT
        t.id AS trade_id,
        SUM(
            (CASE WHEN t.direction = 'BUY'
                  THEN o.filled_price::numeric - t.entry_price
                  ELSE t.entry_price - o.filled_price::numeric
             END) * (o.processed_quantity::numeric / s.lot_size::numeric)
        ) AS exit_pnl
    FROM trades t
    JOIN strategy_market_snapshots s ON s.id = t.strategy_snapshot_id
    JOIN strategy_orders o ON o.trade_id = t.id
    WHERE t.execution_mode = 'demo'
      AND t.status = 'closed'
      AND t.strategy_key = 'futures_breakout_v3'
      AND t.instrument_label = 'GOLDTEN'
      AND s.lot_size IS NOT NULL
      AND s.lot_size > 1
      AND o.role IN ('TARGET', 'SL1', 'SL2')
      AND o.status = 'filled'
      AND o.processed_quantity > 0
      AND o.filled_price IS NOT NULL
    GROUP BY t.id
),
candidates AS (
    SELECT
        t.id,
        t.user_id,
        t.pnl::numeric(18, 2) AS old_pnl,
        ROUND(COALESCE(
            f.exit_pnl,
            (CASE WHEN t.direction = 'BUY'
                  THEN t.exit_price - t.entry_price
                  ELSE t.entry_price - t.exit_price
             END) * GREATEST(
                 COALESCE(NULLIF(t.total_lots, 0), 0),
                 CASE
                     WHEN t.quantity > 0 THEN CEIL(t.quantity::numeric / s.lot_size::numeric)::int
                     ELSE 0
                 END
             )::numeric
        )::numeric, 2) AS corrected_pnl,
        s.lot_size
    FROM trades t
    JOIN strategy_market_snapshots s ON s.id = t.strategy_snapshot_id
    LEFT JOIN exit_fills f ON f.trade_id = t.id
    WHERE t.execution_mode = 'demo'
      AND t.status = 'closed'
      AND t.strategy_key = 'futures_breakout_v3'
      AND t.instrument_label = 'GOLDTEN'
      AND s.lot_size IS NOT NULL
      AND s.lot_size > 1
      AND t.entry_price IS NOT NULL
      AND t.exit_price IS NOT NULL
)
SELECT *, corrected_pnl - old_pnl AS pnl_delta
FROM candidates
WHERE ABS(old_pnl - corrected_pnl * lot_size) < 0.05
  AND ABS(old_pnl - corrected_pnl) > 0.05;

UPDATE trades t
SET pnl = f.corrected_pnl,
    updated_at = NOW(),
    notes = CASE
        WHEN t.notes LIKE '%GOLDTEN demo P&L lot-size correction%'
            THEN t.notes
        ELSE CONCAT(t.notes, '; GOLDTEN demo P&L lot-size correction 2026-07-24')
    END
FROM goldten_demo_pnl_fix f
WHERE t.id = f.id;

UPDATE user_profiles p
SET demo_balance = (p.demo_balance + totals.balance_delta)::numeric(18, 2),
    updated_at = NOW()
FROM (
    SELECT user_id, SUM(pnl_delta)::numeric(18, 2) AS balance_delta
    FROM goldten_demo_pnl_fix
    GROUP BY user_id
) totals
WHERE p.user_id = totals.user_id;
