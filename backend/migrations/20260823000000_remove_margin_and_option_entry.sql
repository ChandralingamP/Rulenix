-- Remove Option Entry v1 without affecting Futures Breakout v3 or
-- SuperTrend Index Options v1.
DELETE FROM strategy_execution_intents
WHERE strategy_key = 'option_entry_v1';

DELETE FROM strategy_signals
WHERE strategy_key = 'option_entry_v1';

-- Unlink the decision first because strategy_orders.risk_decision_id references
-- risk_decisions without ON DELETE CASCADE. The decision itself retains order_id,
-- so it can be removed safely after the order row is gone.
UPDATE strategy_orders orders
SET risk_decision_id = NULL
WHERE orders.snapshot_id IN (
    SELECT id FROM strategy_market_snapshots
    WHERE strategy_key = 'option_entry_v1'
);

WITH deleted_orders AS (
    DELETE FROM strategy_orders orders
    WHERE orders.snapshot_id IN (
        SELECT id FROM strategy_market_snapshots
        WHERE strategy_key = 'option_entry_v1'
    )
    RETURNING orders.id
)
DELETE FROM risk_decisions decisions
USING deleted_orders
WHERE decisions.order_id = deleted_orders.id;

DELETE FROM strategy_reversal_intents
WHERE snapshot_id IN (
    SELECT id FROM strategy_market_snapshots
    WHERE strategy_key = 'option_entry_v1'
);

DELETE FROM trades
WHERE strategy_key = 'option_entry_v1';

DELETE FROM strategy_market_snapshots
WHERE strategy_key = 'option_entry_v1';

DELETE FROM strategy_events
WHERE strategy_key = 'option_entry_v1';

DELETE FROM strategy_scheduler_runs
WHERE strategy_key = 'option_entry_v1';

DELETE FROM user_strategy_configs
WHERE strategy_key = 'option_entry_v1';

DELETE FROM user_strategy_activations
WHERE strategy_key = 'option_entry_v1';

DELETE FROM backtest_runs
WHERE strategy_key = 'option_entry_v1';

DROP TABLE IF EXISTS backtest_option_contracts;

-- Remove margin calculation caches, persisted amounts, simulated funds, and
-- the obsolete risk-limit input. Live insufficient-funds errors now come from
-- the actual Angel One order response; demo entries have no funds gate.
DROP TABLE IF EXISTS broker_margin_estimates;

ALTER TABLE strategy_orders
    DROP COLUMN IF EXISTS margin_required;

ALTER TABLE trades
    DROP COLUMN IF EXISTS margin_required;

ALTER TABLE user_profiles
    DROP COLUMN IF EXISTS demo_balance;

ALTER TABLE risk_limits
    DROP COLUMN IF EXISTS margin_requirement_percent;

UPDATE risk_decisions
SET values = values
    #- '{order,margin_required}'
    #- '{health,margin_available}'
    #- '{limits,margin_requirement_percent}'
WHERE values #> '{order,margin_required}' IS NOT NULL
   OR values #> '{health,margin_available}' IS NOT NULL
   OR values #> '{limits,margin_requirement_percent}' IS NOT NULL;

UPDATE backtest_runs
SET summary = summary
    - 'margin_requirement_percent'
    - 'initial_margin_per_lot'
    - 'initial_margin'
    - 'max_margin_per_lot'
    - 'max_single_trade_margin_used'
    - 'max_margin_used'
    - 'buy_margin_per_lot'
    - 'sell_margin_per_lot'
    - 'calculator_margin_per_lot'
WHERE summary ?| ARRAY[
    'margin_requirement_percent',
    'initial_margin_per_lot',
    'initial_margin',
    'max_margin_per_lot',
    'max_single_trade_margin_used',
    'max_margin_used',
    'buy_margin_per_lot',
    'sell_margin_per_lot',
    'calculator_margin_per_lot'
];
