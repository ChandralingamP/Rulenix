ALTER TABLE user_profiles
    ALTER COLUMN demo_balance SET DEFAULT 200000.00;

UPDATE user_profiles
SET demo_balance = 200000.00,
    updated_at = NOW()
WHERE demo_balance = 2000000.00;

ALTER TABLE user_strategy_configs
    ALTER COLUMN instrument SET DEFAULT 'GOLDTEN';

ALTER TABLE strategy_events
    ALTER COLUMN instrument SET DEFAULT 'GOLDTEN';

UPDATE user_strategy_configs c
SET instrument = 'GOLDTEN',
    updated_at = NOW()
WHERE c.instrument = 'GOLD'
  AND NOT EXISTS (
      SELECT 1
      FROM user_strategy_configs existing
      WHERE existing.user_id = c.user_id
        AND existing.strategy_key = c.strategy_key
        AND existing.instrument = 'GOLDTEN'
  );

UPDATE strategy_market_snapshots s
SET instrument = 'GOLDTEN'
WHERE s.instrument = 'GOLD'
  AND NOT EXISTS (
      SELECT 1
      FROM strategy_market_snapshots existing
      WHERE existing.strategy_key = s.strategy_key
        AND existing.instrument = 'GOLDTEN'
        AND existing.trade_date = s.trade_date
  );

UPDATE strategy_events
SET instrument = 'GOLDTEN'
WHERE instrument = 'GOLD';

UPDATE trades
SET instrument_label = 'GOLDTEN',
    updated_at = NOW()
WHERE strategy_key = 'futures_breakout_v3'
  AND instrument_label = 'GOLD';
