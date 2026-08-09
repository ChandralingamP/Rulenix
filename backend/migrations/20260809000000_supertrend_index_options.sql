ALTER TABLE user_strategy_configs
    ADD COLUMN IF NOT EXISTS target_points DOUBLE PRECISION NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS stop_loss_points DOUBLE PRECISION NOT NULL DEFAULT 0;

UPDATE user_strategy_configs
SET target_points = 40,
    stop_loss_points = 25,
    updated_at = NOW()
WHERE strategy_key = 'supertrend_index_options_v1'
  AND instrument = 'SENSEX'
  AND (target_points <= 0 OR stop_loss_points <= 0);

UPDATE user_strategy_configs
SET target_points = 25,
    stop_loss_points = 15,
    updated_at = NOW()
WHERE strategy_key = 'supertrend_index_options_v1'
  AND instrument = 'NIFTY'
  AND (target_points <= 0 OR stop_loss_points <= 0);
