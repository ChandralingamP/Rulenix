\set ON_ERROR_STOP on

CREATE TEMP TABLE deployment_safety_inventory (
    user_id UUID PRIMARY KEY,
    username TEXT NOT NULL,
    open_live_trades BIGINT NOT NULL,
    unresolved_closed_live_trades BIGINT NOT NULL,
    unresolved_live_orders BIGINT NOT NULL,
    unresolved_live_execution_intents BIGINT NOT NULL,
    unresolved_live_reversals BIGINT NOT NULL,
    unresolved_live_manual_closes BIGINT NOT NULL,
    unresolved_broker_incidents BIGINT NOT NULL,
    unresolved_broker_mutations BIGINT NOT NULL
);

DO $inventory$
BEGIN
    IF to_regclass('public.broker_deployment_account_safety') IS NOT NULL THEN
        EXECUTE $sql$
            INSERT INTO deployment_safety_inventory
            SELECT user_id, username, open_live_trades,
                   unresolved_closed_live_trades, unresolved_live_orders,
                   unresolved_live_execution_intents, unresolved_live_reversals,
                   unresolved_live_manual_closes, unresolved_broker_incidents,
                   unresolved_broker_mutations
            FROM broker_deployment_account_safety
        $sql$;
    ELSE
        INSERT INTO deployment_safety_inventory
        SELECT u.id,
               u.username,
               (SELECT COUNT(*) FROM trades t
                 WHERE t.user_id=u.id AND t.execution_mode='live' AND t.status='open'),
               (SELECT COUNT(*) FROM trades t
                 WHERE t.user_id=u.id AND t.execution_mode='live' AND t.status='closed'
                   AND (COALESCE(t.safety_status,'')<>'CLOSED' OR COALESCE(t.broker_net_quantity,0)<>0)),
               (SELECT COUNT(*) FROM strategy_orders o
                 WHERE o.user_id=u.id AND o.execution_mode='live'
                   AND (o.status IN ('pending','submitting','ambiguous','submitted','partially_filled','processing','cancelling')
                     OR (o.broker_error_class='ambiguous' AND o.status NOT IN ('filled','rejected','cancelled')))),
               (SELECT COUNT(*) FROM strategy_execution_intents i
                 LEFT JOIN strategy_orders o ON o.id=i.strategy_order_id
                 LEFT JOIN trades intent_trade ON intent_trade.id=i.trade_id
                 WHERE i.user_id=u.id AND i.status IN ('pending','claimed','retry_wait','submitted')
                   AND (o.execution_mode='live' OR intent_trade.execution_mode='live'
                     OR (o.id IS NULL AND intent_trade.id IS NULL AND COALESCE(p.trading_mode,'demo')='live'))),
               (SELECT COUNT(*) FROM strategy_reversal_intents r
                 JOIN trades source ON source.id=r.source_trade_id
                 WHERE r.user_id=u.id AND source.execution_mode='live'
                   AND r.status IN ('pending','processing','waiting','submitted','failed')),
               0,
               (SELECT COUNT(*) FROM broker_position_incidents i
                 WHERE i.user_id=u.id AND i.status IN ('open','operator_required')),
               0
        FROM users u
        LEFT JOIN user_profiles p ON p.user_id=u.id;
    END IF;
END
$inventory$;

SELECT COALESCE(json_agg(json_build_object(
    'user_id', account.user_id::TEXT,
    'username', account.username,
    'broker_account_id', profile.broker_account_id,
    'broker_credential_revision', profile.broker_credential_revision,
    'token_state', profile.token_state,
    'last_token_status', profile.last_token_status,
    'last_token_check_at', profile.last_token_check_at,
    'local', json_build_object(
        'open_live_trades', account.open_live_trades,
        'unresolved_closed_live_trades', account.unresolved_closed_live_trades,
        'unresolved_live_orders', account.unresolved_live_orders,
        'unresolved_live_execution_intents', account.unresolved_live_execution_intents,
        'unresolved_live_reversals', account.unresolved_live_reversals,
        'unresolved_live_manual_closes', account.unresolved_live_manual_closes,
        'unresolved_broker_incidents', account.unresolved_broker_incidents
        ,'unresolved_broker_mutations', account.unresolved_broker_mutations
    ),
    'secrets', COALESCE(secrets.values, '{}'::JSON),
    'egress_ip', egress.ip_address,
    'egress_configuration_status', egress.configuration_status,
    'egress_verification_status', egress.verification_status,
    'egress_last_verified_at', egress.last_verified_at
) ORDER BY account.user_id), '[]'::JSON)::TEXT
FROM deployment_safety_inventory account
JOIN LATERAL (
    SELECT p.brokerage_user_id AS broker_account_id,
           p.broker_credential_revision,
           p.token_state,
           p.last_token_status,
           p.last_token_check_at
    FROM user_profiles p
    WHERE p.user_id=account.user_id
) profile ON TRUE
LEFT JOIN LATERAL (
    SELECT json_object_agg(s.secret_kind, json_build_object(
        'version', s.key_version,
        'nonce', encode(s.nonce, 'base64'),
        'ciphertext', encode(s.ciphertext, 'base64'),
        'updated_at', s.updated_at
    )) AS values
    FROM broker_secrets s
    WHERE s.user_id=account.user_id AND s.secret_kind IN ('api_key','jwt_token')
) secrets ON TRUE
LEFT JOIN LATERAL (
    SELECT host(e.ip_address) AS ip_address,
           e.configuration_status,
           e.verification_status,
           e.last_verified_at
    FROM user_profiles p
    JOIN broker_egress_ips e ON e.id=p.broker_egress_ip_id
    WHERE p.user_id=account.user_id
) egress ON TRUE;
