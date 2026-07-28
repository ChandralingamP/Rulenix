DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM trades
        WHERE strategy_key = 'futures_breakout_v3'
          AND instrument_label = 'GOLD'
          AND status = 'open'
    ) THEN
        RAISE EXCEPTION
            'Cannot remove GOLD while a Futures Breakout position is open';
    END IF;

    IF EXISTS (
        SELECT 1
        FROM strategy_orders AS orders
        JOIN strategy_market_snapshots AS snapshot
          ON snapshot.id = orders.snapshot_id
        WHERE snapshot.strategy_key = 'futures_breakout_v3'
          AND snapshot.instrument = 'GOLD'
          AND orders.execution_mode = 'live'
          AND orders.status IN (
              'pending',
              'submitting',
              'ambiguous',
              'submitted',
              'partially_filled',
              'processing',
              'cancelling'
          )
    ) THEN
        RAISE EXCEPTION
            'Cannot remove GOLD while a live Futures Breakout order is active';
    END IF;
END
$$;

UPDATE strategy_orders AS orders
SET status = 'cancelled',
    broker_status = 'Cancelled because GOLD was removed from Futures Breakout',
    updated_at = NOW()
FROM strategy_market_snapshots AS snapshot
WHERE snapshot.id = orders.snapshot_id
  AND snapshot.strategy_key = 'futures_breakout_v3'
  AND snapshot.instrument = 'GOLD'
  AND orders.execution_mode = 'demo'
  AND orders.status IN (
      'pending',
      'submitting',
      'ambiguous',
      'submitted',
      'partially_filled',
      'processing',
      'cancelling'
  );

DELETE FROM user_strategy_configs
WHERE strategy_key = 'futures_breakout_v3'
  AND instrument = 'GOLD';
