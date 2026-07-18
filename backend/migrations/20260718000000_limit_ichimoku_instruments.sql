UPDATE user_strategy_configs
SET enabled = FALSE,
    updated_at = NOW()
WHERE strategy_key = 'ichimoku_keltner_tsi'
  AND instrument NOT IN ('NIFTY', 'SENSEX')
  AND enabled = TRUE;
