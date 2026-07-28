UPDATE strategy_orders AS orders
SET status = 'cancelled',
    broker_status = 'Cancelled during Futures Breakout gap-entry rule upgrade',
    updated_at = NOW()
FROM strategy_market_snapshots AS snapshot
WHERE snapshot.id = orders.snapshot_id
  AND snapshot.strategy_key = 'futures_breakout_v3'
  AND snapshot.gap_plan_status IS NULL
  AND orders.execution_mode = 'demo'
  AND orders.role IN ('BUY_ENTRY', 'SELL_ENTRY')
  AND orders.status IN (
      'pending',
      'submitting',
      'ambiguous',
      'submitted',
      'partially_filled',
      'processing',
      'cancelling'
  );
