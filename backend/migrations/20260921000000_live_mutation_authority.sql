-- Phase 13 LIVE mutation fencing.  The advisory lock and epoch row form one
-- authority boundary shared by Rust, Python, and the cutover operator.
CREATE TABLE live_mutation_authority (
    singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (singleton),
    holder VARCHAR(16) NOT NULL CHECK (holder IN ('rust', 'python', 'none')),
    epoch BIGINT NOT NULL CHECK (epoch > 0),
    lease_owner UUID,
    lease_expires_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_by TEXT NOT NULL DEFAULT '',
    CHECK (
        (holder = 'none' AND lease_owner IS NULL)
        OR (holder <> 'none' AND lease_owner IS NOT NULL)
    )
);

INSERT INTO live_mutation_authority(
    singleton, holder, epoch, lease_owner, lease_expires_at, updated_by
)
VALUES (
    TRUE,
    'rust',
    1,
    '267961f9-6037-580b-906f-152939952a73'::uuid,
    'infinity'::timestamptz,
    'phase13 migration seed: Rust remains authoritative'
)
ON CONFLICT (singleton) DO NOTHING;

CREATE TABLE live_mutation_authority_events (
    id BIGSERIAL PRIMARY KEY,
    previous_holder VARCHAR(16) NOT NULL,
    new_holder VARCHAR(16) NOT NULL,
    previous_epoch BIGINT NOT NULL,
    new_epoch BIGINT NOT NULL,
    previous_lease_owner UUID,
    new_lease_owner UUID,
    reason TEXT NOT NULL,
    changed_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE broker_mutation_attempts (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    strategy_order_id UUID REFERENCES strategy_orders(id) ON DELETE SET NULL,
    execution_intent_id UUID REFERENCES strategy_execution_intents(id) ON DELETE SET NULL,
    trade_id UUID REFERENCES trades(id) ON DELETE SET NULL,
    operation VARCHAR(24) NOT NULL CHECK (operation IN (
        'place_order', 'cancel_order', 'modify_order',
        'gtt_create', 'gtt_modify', 'gtt_cancel'
    )),
    state VARCHAR(24) NOT NULL CHECK (state IN (
        'prepared', 'submitting', 'acknowledged', 'ambiguous',
        'rejected', 'failed', 'cancelled', 'blocked'
    )),
    idempotency_key VARCHAR(192) NOT NULL UNIQUE,
    request_fingerprint VARCHAR(64) NOT NULL,
    client_reference VARCHAR(32) NOT NULL DEFAULT '',
    broker_order_id VARCHAR(96) NOT NULL DEFAULT '',
    authority_holder VARCHAR(16),
    authority_epoch BIGINT,
    authority_lease_owner UUID,
    broker_error_class VARCHAR(32) NOT NULL DEFAULT '',
    broker_error_code VARCHAR(64) NOT NULL DEFAULT '',
    broker_http_status INTEGER,
    diagnostic TEXT NOT NULL DEFAULT '',
    broker_payload JSONB,
    prepared_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    network_started_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX broker_mutation_attempts_unresolved_idx
    ON broker_mutation_attempts(user_id, state, updated_at)
    WHERE state IN ('prepared', 'submitting', 'ambiguous');

CREATE UNIQUE INDEX broker_mutation_attempts_active_order_operation_idx
    ON broker_mutation_attempts(strategy_order_id, operation)
    WHERE strategy_order_id IS NOT NULL
      AND state IN ('prepared', 'submitting', 'acknowledged', 'ambiguous');

COMMENT ON TABLE live_mutation_authority IS
    'Singleton epoch/lease. Mutators hold the shared advisory transaction lock while calling Angel; authority transfer holds the exclusive lock.';
COMMENT ON TABLE broker_mutation_attempts IS
    'Durable broker write lineage persisted before network. Ambiguous outcomes are reconciled and never blindly replayed.';

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
    (
      COALESCE((SELECT COUNT(*) FROM broker_reconciliation_blockers b
                WHERE b.user_id=u.id AND b.status='open'),0)
      +
      COALESCE((SELECT COUNT(*) FROM broker_mutation_attempts a
                WHERE a.user_id=u.id AND a.state IN ('prepared','submitting','ambiguous')),0)
    )::BIGINT AS unresolved_broker_mutations
FROM users u
LEFT JOIN user_profiles p ON p.user_id=u.id;

COMMENT ON VIEW broker_deployment_account_safety IS
    'Per-user Rulenix-owned/ambiguous LIVE state, including durable Phase 13 broker mutations. Proven manual broker exposure remains separate.';
