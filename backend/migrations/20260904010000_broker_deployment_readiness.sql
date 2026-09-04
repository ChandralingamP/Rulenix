ALTER TABLE broker_reconciliation_health
    ADD COLUMN IF NOT EXISTS broker_credential_revision BIGINT;

-- Earlier releases could mark this row healthy after reading only the order
-- book.  Require a new full, revision-bound reconciliation after this release.
UPDATE broker_reconciliation_health
SET healthy = FALSE,
    broker_credential_revision = NULL,
    detail = 'Full broker readiness reconciliation is required after deployment.',
    checked_at = NOW();

CREATE TABLE broker_reconciliation_blockers (
    user_id UUID PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    status VARCHAR(16) NOT NULL DEFAULT 'open'
        CHECK (status IN ('open', 'resolved')),
    external_active_orders BIGINT NOT NULL DEFAULT 0 CHECK (external_active_orders >= 0),
    structurally_unknown_orders BIGINT NOT NULL DEFAULT 0 CHECK (structurally_unknown_orders >= 0),
    active_conditional_rules BIGINT NOT NULL DEFAULT 0 CHECK (active_conditional_rules >= 0),
    detail TEXT NOT NULL DEFAULT '',
    first_detected_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_checked_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    resolved_at TIMESTAMPTZ
);

CREATE INDEX broker_reconciliation_blockers_open_idx
    ON broker_reconciliation_blockers (status, last_checked_at)
    WHERE status = 'open';

CREATE OR REPLACE VIEW broker_deployment_account_safety AS
WITH inventory AS (
SELECT
    u.id AS user_id,
    u.username,
    u.is_active,
    u.can_live_trade,
    COALESCE(p.trading_mode, 'demo') AS trading_mode,
    COALESCE((
        SELECT COUNT(*)
        FROM trades t
        WHERE t.user_id = u.id
          AND t.execution_mode = 'live'
          AND t.status = 'open'
    ), 0)::BIGINT AS open_live_trades,
    COALESCE((
        SELECT COUNT(*)
        FROM trades t
        WHERE t.user_id = u.id
          AND t.execution_mode = 'live'
          AND t.status = 'closed'
          AND (
              COALESCE(t.safety_status, '') <> 'CLOSED'
              OR COALESCE(t.broker_net_quantity, 0) <> 0
          )
    ), 0)::BIGINT AS unresolved_closed_live_trades,
    COALESCE((
        SELECT COUNT(*)
        FROM strategy_orders o
        WHERE o.user_id = u.id
          AND o.execution_mode = 'live'
          AND (
              o.status IN (
                  'pending', 'submitting', 'ambiguous', 'submitted',
                  'partially_filled', 'processing', 'cancelling'
              )
              OR (
                  o.broker_error_class = 'ambiguous'
                  AND o.status NOT IN ('filled', 'rejected', 'cancelled')
              )
          )
    ), 0)::BIGINT AS unresolved_live_orders,
    COALESCE((
        SELECT COUNT(*)
        FROM strategy_execution_intents i
        LEFT JOIN strategy_orders o ON o.id = i.strategy_order_id
        LEFT JOIN trades intent_trade ON intent_trade.id = i.trade_id
        WHERE i.user_id = u.id
          AND i.status IN ('pending', 'claimed', 'retry_wait', 'submitted')
          AND (
              o.execution_mode = 'live'
              OR intent_trade.execution_mode = 'live'
              OR (
                  o.id IS NULL
                  AND intent_trade.id IS NULL
                  AND COALESCE(p.trading_mode, 'demo') = 'live'
              )
          )
    ), 0)::BIGINT AS unresolved_live_execution_intents,
    COALESCE((
        SELECT COUNT(*)
        FROM strategy_reversal_intents r
        JOIN trades source ON source.id = r.source_trade_id
        WHERE r.user_id = u.id
          AND source.execution_mode = 'live'
          AND r.status IN ('pending', 'processing', 'waiting', 'submitted', 'failed')
    ), 0)::BIGINT AS unresolved_live_reversals,
    COALESCE((
        SELECT COUNT(*)
        FROM manual_trade_close_intents m
        JOIN trades t ON t.id = m.trade_id
        WHERE m.user_id = u.id
          AND t.execution_mode = 'live'
          AND m.status <> 'completed'
    ), 0)::BIGINT AS unresolved_live_manual_closes,
    COALESCE((
        SELECT COUNT(*)
        FROM broker_position_incidents i
        WHERE i.user_id = u.id
          AND i.status IN ('open', 'operator_required')
    ), 0)::BIGINT AS unresolved_broker_incidents,
    COALESCE((
        SELECT COUNT(*)
        FROM broker_reconciliation_blockers b
        WHERE b.user_id = u.id
          AND b.status = 'open'
    ), 0)::BIGINT AS unresolved_broker_mutations
FROM users u
LEFT JOIN user_profiles p ON p.user_id = u.id
)
SELECT * FROM inventory;

COMMENT ON VIEW broker_deployment_account_safety IS
    'Durable per-user LIVE exposure inventory used by fail-closed deployment gates. An unreadable broker account is deployment-safe only when every unresolved count is zero; users without linked broker credentials remain LIVE-ineligible.';
