\set ON_ERROR_STOP on

CREATE TEMP TABLE deployment_safety_inventory (
    user_id UUID PRIMARY KEY,
    username TEXT NOT NULL,
    is_active BOOLEAN NOT NULL,
    can_live_trade BOOLEAN NOT NULL,
    trading_mode TEXT NOT NULL,
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
            SELECT user_id, username, is_active, can_live_trade, trading_mode, open_live_trades,
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
               u.is_active,
               u.can_live_trade,
               COALESCE(p.trading_mode,'demo'),
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
    'is_active', account.is_active,
    'can_live_trade', account.can_live_trade,
    'trading_mode', account.trading_mode,
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
    'known_orders', COALESCE(orders.values, '[]'::JSON),
    'open_local_positions', COALESCE(positions.values, '[]'::JSON),
    'rulenix_contract_history', COALESCE(contract_history.values, '[]'::JSON),
    'secrets', COALESCE(secrets.values, '{}'::JSON),
    'egress_ip', egress.ip_address,
    'egress_configuration_status', egress.configuration_status,
    'egress_verification_status', egress.verification_status
) ORDER BY account.user_id), '[]'::JSON)::TEXT
FROM deployment_safety_inventory account
LEFT JOIN LATERAL (
    SELECT json_object_agg(s.secret_kind, json_build_object(
        'version', s.key_version,
        'nonce', encode(s.nonce, 'base64'),
        'ciphertext', encode(s.ciphertext, 'base64')
    )) AS values
    FROM broker_secrets s
    WHERE s.user_id=account.user_id AND s.secret_kind IN ('api_key','jwt_token')
) secrets ON TRUE
LEFT JOIN LATERAL (
    SELECT json_agg(json_build_object(
        'broker_order_id', o.broker_order_id,
        'client_order_id', o.client_order_id,
        'exchange_segment', o.exchange_segment,
        'contract_token', s.contract_token,
        'status', o.status
    )) AS values
    FROM strategy_orders o
    LEFT JOIN strategy_market_snapshots s ON s.id=o.snapshot_id
    WHERE o.user_id=account.user_id AND o.execution_mode='live'
      AND (o.broker_order_id<>'' OR o.client_order_id<>'')
) orders ON TRUE
LEFT JOIN LATERAL (
    SELECT json_agg(json_build_object(
        'exchange_segment', UPPER(s.exchange_segment),
        'contract_token', s.contract_token,
        'contract_symbol', COALESCE(t.contract_symbol,''),
        'direction', t.direction,
        'quantity', t.quantity
    )) AS values
    FROM trades t
    JOIN strategy_market_snapshots s ON s.id=t.strategy_snapshot_id
    WHERE t.user_id=account.user_id AND t.execution_mode='live'
      AND t.status='open' AND s.contract_token IS NOT NULL
) positions ON TRUE
LEFT JOIN LATERAL (
    SELECT json_agg(json_build_object(
        'exchange_segment', evidence.exchange_segment,
        'contract_token', evidence.contract_token,
        'evidence_rows', evidence.evidence_rows
    )) AS values
    FROM (
        SELECT exchange_segment, contract_token, COUNT(*)::BIGINT AS evidence_rows
        FROM (
            SELECT UPPER(s.exchange_segment) AS exchange_segment, s.contract_token
            FROM trades t JOIN strategy_market_snapshots s ON s.id=t.strategy_snapshot_id
            WHERE t.user_id=account.user_id AND t.execution_mode='live' AND s.contract_token IS NOT NULL
            UNION ALL
            SELECT UPPER(s.exchange_segment), s.contract_token
            FROM strategy_orders o JOIN strategy_market_snapshots s ON s.id=o.snapshot_id
            WHERE o.user_id=account.user_id AND o.execution_mode='live' AND s.contract_token IS NOT NULL
            UNION ALL
            SELECT UPPER(s.exchange_segment), s.contract_token
            FROM strategy_execution_intents i JOIN strategy_market_snapshots s ON s.id=i.snapshot_id
            WHERE i.user_id=account.user_id AND s.contract_token IS NOT NULL
            UNION ALL
            SELECT UPPER(s.exchange_segment), s.contract_token
            FROM strategy_execution_intents i JOIN strategy_orders o ON o.id=i.strategy_order_id
            JOIN strategy_market_snapshots s ON s.id=o.snapshot_id
            WHERE i.user_id=account.user_id AND s.contract_token IS NOT NULL
            UNION ALL
            SELECT UPPER(s.exchange_segment), s.contract_token
            FROM strategy_execution_intents i JOIN trades t ON t.id=i.trade_id
            JOIN strategy_market_snapshots s ON s.id=t.strategy_snapshot_id
            WHERE i.user_id=account.user_id AND s.contract_token IS NOT NULL
            UNION ALL
            SELECT UPPER(s.exchange_segment), s.contract_token
            FROM strategy_reversal_intents r JOIN strategy_market_snapshots s ON s.id=r.snapshot_id
            WHERE r.user_id=account.user_id AND s.contract_token IS NOT NULL
            UNION ALL
            SELECT UPPER(s.exchange_segment), s.contract_token
            FROM manual_trade_close_intents m JOIN trades t ON t.id=m.trade_id
            JOIN strategy_market_snapshots s ON s.id=t.strategy_snapshot_id
            WHERE m.user_id=account.user_id AND s.contract_token IS NOT NULL
            UNION ALL
            SELECT UPPER(i.exchange_segment), i.contract_token
            FROM broker_position_incidents i
            WHERE i.user_id=account.user_id AND i.contract_token<>''
        ) durable
        GROUP BY exchange_segment, contract_token
    ) evidence
) contract_history ON TRUE
LEFT JOIN LATERAL (
    SELECT host(e.ip_address) AS ip_address,
           e.configuration_status,
           e.verification_status
    FROM user_profiles p
    JOIN broker_egress_ips e ON e.id=p.broker_egress_ip_id
    WHERE p.user_id=account.user_id
) egress ON TRUE;
