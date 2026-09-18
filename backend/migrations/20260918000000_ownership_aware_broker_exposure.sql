ALTER TABLE broker_position_incidents
    DROP CONSTRAINT IF EXISTS broker_position_incidents_ownership_status_check;

UPDATE broker_position_incidents
SET ownership_status = 'ambiguous'
WHERE ownership_status = 'detected_unattributed';

ALTER TABLE broker_position_incidents
    ADD CONSTRAINT broker_position_incidents_ownership_status_check
    CHECK (ownership_status IN ('strategy_related', 'manual_external', 'ambiguous'));

CREATE TABLE broker_exposure_observations (
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    exposure_kind VARCHAR(16) NOT NULL
        CHECK (exposure_kind IN ('position', 'order', 'conditional')),
    broker_reference TEXT NOT NULL,
    ownership_status VARCHAR(24) NOT NULL
        CHECK (ownership_status IN ('rulenix_owned', 'manual_external', 'ambiguous')),
    exchange_segment VARCHAR(16) NOT NULL DEFAULT '',
    contract_token TEXT NOT NULL DEFAULT '',
    contract_symbol TEXT NOT NULL DEFAULT '',
    side VARCHAR(8) NOT NULL DEFAULT '',
    quantity INTEGER NOT NULL DEFAULT 0,
    evidence TEXT NOT NULL DEFAULT '',
    broker_credential_revision BIGINT NOT NULL,
    observed_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (user_id, exposure_kind, broker_reference)
);

CREATE INDEX broker_exposure_observations_contract_idx
    ON broker_exposure_observations
       (user_id, exchange_segment, contract_token, ownership_status, observed_at);

ALTER TABLE broker_reconciliation_blockers
    ADD COLUMN IF NOT EXISTS rulenix_owned_exposure BIGINT NOT NULL DEFAULT 0
        CHECK (rulenix_owned_exposure >= 0),
    ADD COLUMN IF NOT EXISTS ambiguous_exposure BIGINT NOT NULL DEFAULT 0
        CHECK (ambiguous_exposure >= 0),
    ADD COLUMN IF NOT EXISTS manual_external_exposure BIGINT NOT NULL DEFAULT 0
        CHECK (manual_external_exposure >= 0);

CREATE OR REPLACE VIEW broker_deployment_account_safety AS
SELECT
    u.id AS user_id,
    u.username,
    u.is_active,
    u.can_live_trade,
    COALESCE(p.trading_mode, 'demo') AS trading_mode,
    COALESCE((SELECT COUNT(*) FROM trades t
              WHERE t.user_id=u.id AND t.execution_mode='live' AND t.status='open'),0)::BIGINT AS open_live_trades,
    COALESCE((SELECT COUNT(*) FROM trades t
              WHERE t.user_id=u.id AND t.execution_mode='live' AND t.status='closed'
                AND (COALESCE(t.safety_status,'')<>'CLOSED' OR COALESCE(t.broker_net_quantity,0)<>0)),0)::BIGINT AS unresolved_closed_live_trades,
    COALESCE((SELECT COUNT(*) FROM strategy_orders o
              WHERE o.user_id=u.id AND o.execution_mode='live'
                AND (o.status IN ('pending','submitting','ambiguous','submitted','partially_filled','processing','cancelling')
                  OR (o.broker_error_class='ambiguous' AND o.status NOT IN ('filled','rejected','cancelled')))),0)::BIGINT AS unresolved_live_orders,
    COALESCE((SELECT COUNT(*) FROM strategy_execution_intents i
              LEFT JOIN strategy_orders o ON o.id=i.strategy_order_id
              LEFT JOIN trades t ON t.id=i.trade_id
              WHERE i.user_id=u.id AND i.status IN ('pending','claimed','retry_wait','submitted')
                AND (o.execution_mode='live' OR t.execution_mode='live'
                  OR (o.id IS NULL AND t.id IS NULL AND COALESCE(p.trading_mode,'demo')='live'))),0)::BIGINT AS unresolved_live_execution_intents,
    COALESCE((SELECT COUNT(*) FROM strategy_reversal_intents r JOIN trades t ON t.id=r.source_trade_id
              WHERE r.user_id=u.id AND t.execution_mode='live'
                AND r.status IN ('pending','processing','waiting','submitted','failed')),0)::BIGINT AS unresolved_live_reversals,
    COALESCE((SELECT COUNT(*) FROM manual_trade_close_intents m JOIN trades t ON t.id=m.trade_id
              WHERE m.user_id=u.id AND t.execution_mode='live' AND m.status<>'completed'),0)::BIGINT AS unresolved_live_manual_closes,
    COALESCE((SELECT COUNT(*) FROM broker_position_incidents i
              WHERE i.user_id=u.id AND i.status IN ('open','operator_required')
                AND i.ownership_status<>'manual_external'),0)::BIGINT AS unresolved_broker_incidents,
    COALESCE((SELECT COUNT(*) FROM broker_reconciliation_blockers b
              WHERE b.user_id=u.id AND b.status='open'),0)::BIGINT AS unresolved_broker_mutations
FROM users u
LEFT JOIN user_profiles p ON p.user_id=u.id;

COMMENT ON TABLE broker_exposure_observations IS
    'Latest authoritative broker exposure inventory. Manual rows are never adopted or mutated; ambiguous rows fail closed.';

COMMENT ON VIEW broker_deployment_account_safety IS
    'Per-user Rulenix-owned and ambiguous LIVE state used by deployment gates. Proven manual broker exposure is reported separately and is not a deployment blocker.';
