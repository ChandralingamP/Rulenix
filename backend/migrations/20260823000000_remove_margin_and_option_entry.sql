-- Retire Option Entry v1 and the legacy persisted margin model from active
-- application use without destroying their historical records.
--
-- The executable strategy/router no longer dispatches `option_entry_v1`, and
-- current live margin validation uses Angel One RMS plus the broker margin
-- endpoint. Database history is intentionally retained in place so original
-- identifiers, timestamps, broker references, fills, P&L, and foreign-key
-- relationships remain queryable and auditable.
--
-- Do not delete legacy rows or drop legacy tables/columns here. They are
-- historical compatibility storage. A future archival migration, if ever
-- needed, must copy and verify every relationship before changing this schema.

COMMENT ON TABLE broker_margin_estimates IS
    'Historical broker margin estimates retained for audit compatibility; active entries use live Angel One margin and RMS validation.';

COMMENT ON TABLE backtest_option_contracts IS
    'Historical Option Entry v1 contract snapshots retained for reproducible backtest and audit history; not used by active execution.';

COMMENT ON COLUMN strategy_orders.margin_required IS
    'Historical estimated margin captured by retired execution paths; retained for order audit compatibility.';

COMMENT ON COLUMN trades.margin_required IS
    'Historical estimated margin captured by retired execution paths; retained for trade and P&L audit compatibility.';

COMMENT ON COLUMN user_profiles.demo_balance IS
    'Historical demo-account balance retained for audit compatibility; no longer used as an active entry funds gate.';

COMMENT ON COLUMN risk_limits.margin_requirement_percent IS
    'Historical risk-limit input retained for audit compatibility; active live entries use broker-reported margin and RMS funds.';
