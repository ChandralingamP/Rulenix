-- Rulenix Unified Database Initialization


-- >>> START: backend/migrations/20260701000000_init.sql <<<
CREATE EXTENSION IF NOT EXISTS pgcrypto;
CREATE TABLE users (
 id UUID PRIMARY KEY DEFAULT gen_random_uuid(), username VARCHAR(150) NOT NULL,
 email VARCHAR(255) NOT NULL, password_hash TEXT NOT NULL,
 is_active BOOLEAN NOT NULL DEFAULT TRUE, is_staff BOOLEAN NOT NULL DEFAULT FALSE,
 is_superuser BOOLEAN NOT NULL DEFAULT FALSE,
 created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(), updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE UNIQUE INDEX users_username_ci_unique ON users (LOWER(username));
CREATE UNIQUE INDEX users_email_ci_unique ON users (LOWER(email));
CREATE TABLE user_profiles (
 user_id UUID PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
 brokerage_user_id VARCHAR(64), api_key VARCHAR(128), mobile_number VARCHAR(16),
 trading_mode VARCHAR(8) NOT NULL DEFAULT 'demo' CHECK (trading_mode IN ('demo','live')),
 jwt_token TEXT, refresh_token TEXT, feed_token TEXT, token_state VARCHAR(64),
 token_received_at TIMESTAMPTZ, last_token_check_at TIMESTAMPTZ,
 last_token_status VARCHAR(64), last_token_message VARCHAR(255),
 created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(), updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE TABLE email_otps (
 id UUID PRIMARY KEY DEFAULT gen_random_uuid(), email VARCHAR(255) NOT NULL,
 otp_hash VARCHAR(128) NOT NULL, purpose VARCHAR(20) NOT NULL CHECK (purpose IN ('signup','password_reset')),
 is_used BOOLEAN NOT NULL DEFAULT FALSE, created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(), expires_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX email_otps_email_used_idx ON email_otps (LOWER(email), is_used);
CREATE INDEX email_otps_email_purpose_used_idx ON email_otps (LOWER(email), purpose, is_used);

-- <<< END: backend/migrations/20260701000000_init.sql >>>


-- >>> START: backend/migrations/20260701030000_rulenix_operations.sql <<<
CREATE TABLE IF NOT EXISTS users (
    id UUID PRIMARY KEY,
    username VARCHAR(150) NOT NULL,
    email VARCHAR(254) NOT NULL,
    password_hash TEXT NOT NULL,
    is_staff BOOLEAN NOT NULL DEFAULT FALSE,
    is_superuser BOOLEAN NOT NULL DEFAULT FALSE,
    is_active BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE UNIQUE INDEX IF NOT EXISTS users_username_lower_uq ON users (LOWER(username));
CREATE UNIQUE INDEX IF NOT EXISTS users_email_lower_uq ON users (LOWER(email));

CREATE TABLE IF NOT EXISTS user_profiles (
    user_id UUID PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    brokerage_user_id VARCHAR(64) NOT NULL,
    api_key VARCHAR(128) NOT NULL,
    mobile_number VARCHAR(16) NOT NULL,
    jwt_token TEXT NOT NULL DEFAULT '',
    refresh_token TEXT NOT NULL DEFAULT '',
    feed_token TEXT NOT NULL DEFAULT '',
    token_state VARCHAR(64) NOT NULL DEFAULT '',
    token_received_at TIMESTAMPTZ,
    last_token_check_at TIMESTAMPTZ,
    last_token_status VARCHAR(64) NOT NULL DEFAULT '',
    last_token_message VARCHAR(255) NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

UPDATE user_profiles SET
    brokerage_user_id = COALESCE(brokerage_user_id, ''),
    api_key = COALESCE(api_key, ''),
    mobile_number = COALESCE(mobile_number, ''),
    jwt_token = COALESCE(jwt_token, ''),
    refresh_token = COALESCE(refresh_token, ''),
    feed_token = COALESCE(feed_token, ''),
    token_state = COALESCE(token_state, ''),
    last_token_status = COALESCE(last_token_status, ''),
    last_token_message = COALESCE(last_token_message, '');

ALTER TABLE user_profiles
    ALTER COLUMN brokerage_user_id SET DEFAULT '',
    ALTER COLUMN brokerage_user_id SET NOT NULL,
    ALTER COLUMN api_key SET DEFAULT '',
    ALTER COLUMN api_key SET NOT NULL,
    ALTER COLUMN mobile_number SET DEFAULT '',
    ALTER COLUMN mobile_number SET NOT NULL,
    ALTER COLUMN jwt_token SET DEFAULT '',
    ALTER COLUMN jwt_token SET NOT NULL,
    ALTER COLUMN refresh_token SET DEFAULT '',
    ALTER COLUMN refresh_token SET NOT NULL,
    ALTER COLUMN feed_token SET DEFAULT '',
    ALTER COLUMN feed_token SET NOT NULL,
    ALTER COLUMN token_state SET DEFAULT '',
    ALTER COLUMN token_state SET NOT NULL,
    ALTER COLUMN last_token_status SET DEFAULT '',
    ALTER COLUMN last_token_status SET NOT NULL,
    ALTER COLUMN last_token_message SET DEFAULT '',
    ALTER COLUMN last_token_message SET NOT NULL;

CREATE TABLE IF NOT EXISTS email_otps (
    id UUID PRIMARY KEY,
    email VARCHAR(254) NOT NULL,
    otp_hash VARCHAR(64) NOT NULL,
    purpose VARCHAR(32) NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    is_used BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS email_otps_lookup_idx ON email_otps (LOWER(email), purpose, is_used, created_at DESC);

CREATE TABLE IF NOT EXISTS trades (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    execution_mode VARCHAR(8) NOT NULL DEFAULT 'demo' CHECK (execution_mode IN ('demo', 'live')),
    status VARCHAR(16) NOT NULL DEFAULT 'open',
    direction VARCHAR(4) NOT NULL CHECK (direction IN ('BUY', 'SELL')),
    quantity INTEGER NOT NULL DEFAULT 0,
    entry_price NUMERIC(18,2),
    exit_price NUMERIC(18,2),
    last_price NUMERIC(18,2),
    pnl NUMERIC(18,2) NOT NULL DEFAULT 0,
    entry_datetime TIMESTAMPTZ,
    exit_datetime TIMESTAMPTZ,
    instrument_label VARCHAR(64) NOT NULL DEFAULT '',
    contract_symbol VARCHAR(96) NOT NULL DEFAULT '',
    external_entry_id VARCHAR(96) NOT NULL DEFAULT '',
    external_exit_id VARCHAR(96) NOT NULL DEFAULT '',
    notes TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS trades_user_entry_idx ON trades (user_id, entry_datetime DESC, created_at DESC);

CREATE TABLE IF NOT EXISTS job_runs (
    id UUID PRIMARY KEY,
    job_key VARCHAR(64) NOT NULL,
    status VARCHAR(16) NOT NULL,
    started_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    completed_at TIMESTAMPTZ,
    error TEXT
);
CREATE INDEX IF NOT EXISTS job_runs_key_idx ON job_runs (job_key, started_at DESC);

-- <<< END: backend/migrations/20260701030000_rulenix_operations.sql >>>


-- >>> START: backend/migrations/20260701040000_account_settings.sql <<<
ALTER TABLE user_profiles
    ADD COLUMN IF NOT EXISTS demo_balance NUMERIC(18,2) NOT NULL DEFAULT 200000.00;

UPDATE user_profiles SET demo_balance = 200000.00 WHERE demo_balance IS NULL;

ALTER TABLE email_otps DROP CONSTRAINT IF EXISTS email_otps_purpose_check;
ALTER TABLE email_otps
    ADD CONSTRAINT email_otps_purpose_check
    CHECK (purpose IN ('signup', 'password_reset', 'profile_update'));

-- <<< END: backend/migrations/20260701040000_account_settings.sql >>>


-- >>> START: backend/migrations/20260702000000_futures_breakout_strategy.sql <<<
CREATE TABLE IF NOT EXISTS strategy_market_snapshots (
    id UUID PRIMARY KEY,
    strategy_key VARCHAR(64) NOT NULL,
    instrument VARCHAR(32) NOT NULL,
    trade_date DATE NOT NULL,
    status VARCHAR(16) NOT NULL CHECK (status IN ('ready', 'missing', 'failed')),
    error TEXT,
    contract_token VARCHAR(32),
    contract_symbol VARCHAR(96),
    contract_expiry DATE,
    lot_size INTEGER,
    candle_dates DATE[] NOT NULL DEFAULT '{}',
    highs DOUBLE PRECISION[] NOT NULL DEFAULT '{}',
    lows DOUBLE PRECISION[] NOT NULL DEFAULT '{}',
    hh2 DOUBLE PRECISION,
    ll2 DOUBLE PRECISION,
    hh4 DOUBLE PRECISION,
    ll4 DOUBLE PRECISION,
    buy_entry DOUBLE PRECISION,
    buy_target DOUBLE PRECISION,
    buy_sl1 DOUBLE PRECISION,
    buy_sl2 DOUBLE PRECISION,
    sell_entry DOUBLE PRECISION,
    sell_target DOUBLE PRECISION,
    sell_sl1 DOUBLE PRECISION,
    sell_sl2 DOUBLE PRECISION,
    fetched_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (strategy_key, instrument, trade_date)
);
CREATE INDEX IF NOT EXISTS strategy_snapshots_lookup_idx
    ON strategy_market_snapshots (instrument, trade_date DESC);

CREATE TABLE IF NOT EXISTS user_strategy_configs (
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    strategy_key VARCHAR(64) NOT NULL DEFAULT 'futures_breakout_v3',
    instrument VARCHAR(32) NOT NULL DEFAULT 'GOLDTEN',
    enabled BOOLEAN NOT NULL DEFAULT FALSE,
    lots INTEGER NOT NULL DEFAULT 1 CHECK (lots > 0),
    run_day_session BOOLEAN NOT NULL DEFAULT TRUE,
    run_evening_session BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (user_id, strategy_key, instrument)
);

CREATE TABLE IF NOT EXISTS strategy_orders (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    snapshot_id UUID NOT NULL REFERENCES strategy_market_snapshots(id),
    trade_id UUID REFERENCES trades(id) ON DELETE SET NULL,
    session_key VARCHAR(32) NOT NULL,
    role VARCHAR(16) NOT NULL CHECK (role IN ('BUY_ENTRY','SELL_ENTRY','TARGET','SL1','SL2')),
    side VARCHAR(4) NOT NULL CHECK (side IN ('BUY','SELL')),
    execution_mode VARCHAR(8) NOT NULL CHECK (execution_mode IN ('demo','live')),
    lots INTEGER NOT NULL CHECK (lots > 0),
    quantity INTEGER NOT NULL CHECK (quantity > 0),
    price DOUBLE PRECISION NOT NULL,
    trigger_price DOUBLE PRECISION,
    status VARCHAR(16) NOT NULL DEFAULT 'pending',
    broker_order_id VARCHAR(96) NOT NULL DEFAULT '',
    broker_status TEXT NOT NULL DEFAULT '',
    idempotency_key VARCHAR(192) NOT NULL UNIQUE,
    filled_price DOUBLE PRECISION,
    filled_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS strategy_orders_reconcile_idx
    ON strategy_orders (user_id, execution_mode, status, created_at);

CREATE TABLE IF NOT EXISTS strategy_events (
    id BIGSERIAL PRIMARY KEY,
    user_id UUID REFERENCES users(id) ON DELETE CASCADE,
    strategy_key VARCHAR(64) NOT NULL DEFAULT 'futures_breakout_v3',
    instrument VARCHAR(32) NOT NULL DEFAULT 'GOLDTEN',
    event_type VARCHAR(48) NOT NULL,
    payload JSONB NOT NULL DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS strategy_events_user_idx
    ON strategy_events (user_id, id DESC);

ALTER TABLE trades ADD COLUMN IF NOT EXISTS strategy_key VARCHAR(64) NOT NULL DEFAULT '';
ALTER TABLE trades ADD COLUMN IF NOT EXISTS strategy_snapshot_id UUID REFERENCES strategy_market_snapshots(id);
ALTER TABLE trades ADD COLUMN IF NOT EXISTS total_lots INTEGER NOT NULL DEFAULT 0;
ALTER TABLE trades ADD COLUMN IF NOT EXISTS remaining_lots INTEGER NOT NULL DEFAULT 0;
ALTER TABLE trades ADD COLUMN IF NOT EXISTS target_price DOUBLE PRECISION;
ALTER TABLE trades ADD COLUMN IF NOT EXISTS sl1_price DOUBLE PRECISION;
ALTER TABLE trades ADD COLUMN IF NOT EXISTS sl2_price DOUBLE PRECISION;

-- <<< END: backend/migrations/20260702000000_futures_breakout_strategy.sql >>>


-- >>> START: backend/migrations/20260702010000_strategy_catalog.sql <<<
CREATE TABLE IF NOT EXISTS user_strategy_activations (
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    strategy_key VARCHAR(64) NOT NULL,
    is_active BOOLEAN NOT NULL DEFAULT FALSE,
    activated_at TIMESTAMPTZ,
    deactivated_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (user_id, strategy_key)
);

INSERT INTO user_strategy_activations (user_id, strategy_key, is_active, activated_at)
SELECT DISTINCT user_id, strategy_key, TRUE, NOW()
FROM user_strategy_configs
WHERE enabled = TRUE
ON CONFLICT (user_id, strategy_key) DO NOTHING;

-- <<< END: backend/migrations/20260702010000_strategy_catalog.sql >>>


-- >>> START: backend/migrations/20260702020000_demo_balance_accounting.sql <<<
ALTER TABLE user_profiles
    ALTER COLUMN demo_balance SET DEFAULT 200000.00;

-- Keep existing demo accounts at the configured starting balance unless users
-- have already topped up or changed them.
UPDATE user_profiles
SET demo_balance = 200000.00,
    updated_at = NOW()
WHERE demo_balance IS NULL;

-- <<< END: backend/migrations/20260702020000_demo_balance_accounting.sql >>>


-- >>> START: backend/migrations/20260702030000_strategy_resilience.sql <<<
CREATE TABLE IF NOT EXISTS strategy_scheduler_runs (
    id UUID PRIMARY KEY,
    strategy_key VARCHAR(64) NOT NULL,
    instrument VARCHAR(32) NOT NULL,
    trade_date DATE NOT NULL,
    session_key VARCHAR(16) NOT NULL CHECK (session_key IN ('day', 'evening')),
    action VARCHAR(16) NOT NULL CHECK (action IN ('target', 'entry')),
    status VARCHAR(16) NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'running', 'completed', 'failed', 'skipped')),
    attempts INTEGER NOT NULL DEFAULT 0,
    scheduled_for TIMESTAMPTZ NOT NULL,
    next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    started_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    last_error TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (strategy_key, instrument, trade_date, session_key, action)
);
CREATE INDEX IF NOT EXISTS strategy_scheduler_runs_due_idx
    ON strategy_scheduler_runs (status, next_attempt_at);

CREATE TABLE IF NOT EXISTS market_calendar (
    trade_date DATE PRIMARY KEY,
    morning_open BOOLEAN NOT NULL DEFAULT TRUE,
    evening_open BOOLEAN NOT NULL DEFAULT TRUE,
    reason VARCHAR(128) NOT NULL DEFAULT '',
    source VARCHAR(255) NOT NULL DEFAULT '',
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- MCX 2026 trading holidays. Session-specific closures matter because several
-- holidays close only the morning or evening commodity session.
INSERT INTO market_calendar (trade_date, morning_open, evening_open, reason, source) VALUES
('2026-01-01', TRUE,  FALSE, 'New Year Day', 'MCX trading holidays 2026'),
('2026-01-26', FALSE, FALSE, 'Republic Day', 'MCX trading holidays 2026'),
('2026-03-03', FALSE, TRUE,  'Holi', 'MCX trading holidays 2026'),
('2026-03-26', FALSE, TRUE,  'Shri Ram Navmi', 'MCX trading holidays 2026'),
('2026-03-31', FALSE, TRUE,  'Shri Mahavir Jayanti', 'MCX trading holidays 2026'),
('2026-04-03', FALSE, FALSE, 'Good Friday', 'MCX trading holidays 2026'),
('2026-04-14', FALSE, TRUE,  'Dr. Baba Saheb Ambedkar Jayanti', 'MCX trading holidays 2026'),
('2026-05-01', FALSE, TRUE,  'Maharashtra Day', 'MCX trading holidays 2026'),
('2026-05-28', FALSE, TRUE,  'Bakri Id', 'MCX trading holidays 2026'),
('2026-06-26', FALSE, TRUE,  'Moharram', 'MCX trading holidays 2026'),
('2026-09-14', FALSE, TRUE,  'Ganesh Chaturthi', 'MCX trading holidays 2026'),
('2026-10-02', FALSE, FALSE, 'Mahatma Gandhi Jayanti', 'MCX trading holidays 2026'),
('2026-10-20', FALSE, TRUE,  'Dassera', 'MCX trading holidays 2026'),
('2026-11-10', FALSE, TRUE,  'Diwali-Balipratipada', 'MCX trading holidays 2026'),
('2026-11-24', FALSE, TRUE,  'Guru Nanak Jayanti', 'MCX trading holidays 2026'),
('2026-12-25', FALSE, FALSE, 'Christmas', 'MCX trading holidays 2026')
ON CONFLICT (trade_date) DO NOTHING;

-- <<< END: backend/migrations/20260702030000_strategy_resilience.sql >>>


-- >>> START: backend/migrations/20260702040000_secure_sessions.sql <<<
ALTER TABLE users ADD COLUMN IF NOT EXISTS password_changed_at TIMESTAMPTZ NOT NULL DEFAULT NOW();

CREATE TABLE user_sessions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    token_hash BYTEA NOT NULL UNIQUE,
    csrf_hash BYTEA NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_seen_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    idle_expires_at TIMESTAMPTZ NOT NULL,
    absolute_expires_at TIMESTAMPTZ NOT NULL,
    revoked_at TIMESTAMPTZ,
    user_agent VARCHAR(512),
    ip_address INET
);
CREATE INDEX user_sessions_user_active_idx ON user_sessions (user_id, absolute_expires_at) WHERE revoked_at IS NULL;
CREATE INDEX user_sessions_expiry_idx ON user_sessions (idle_expires_at, absolute_expires_at);

-- <<< END: backend/migrations/20260702040000_secure_sessions.sql >>>


-- >>> START: backend/migrations/20260702050000_rbac_trading_mode.sql <<<
-- Administrative authority and trading authority are deliberately independent.
ALTER TABLE users ADD COLUMN IF NOT EXISTS can_administer BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE users ADD COLUMN IF NOT EXISTS can_live_trade BOOLEAN NOT NULL DEFAULT FALSE;

-- Preserve existing staff administration, but never infer live-trading authority.
UPDATE users SET can_administer = is_staff WHERE is_staff = TRUE;
UPDATE users SET can_live_trade = FALSE;
UPDATE user_profiles SET trading_mode = 'demo';

CREATE INDEX IF NOT EXISTS users_can_administer_idx ON users (can_administer) WHERE can_administer = TRUE;
CREATE INDEX IF NOT EXISTS users_can_live_trade_idx ON users (can_live_trade) WHERE can_live_trade = TRUE;

COMMENT ON COLUMN users.can_administer IS 'May administer users and scheduler operations.';
COMMENT ON COLUMN users.can_live_trade IS 'May explicitly select live trading after broker validation.';
COMMENT ON COLUMN user_profiles.trading_mode IS 'Selected execution mode; independent of administrative permissions.';

-- <<< END: backend/migrations/20260702050000_rbac_trading_mode.sql >>>


-- >>> START: backend/migrations/20260702060000_encrypted_broker_secrets.sql <<<
CREATE TABLE broker_secrets (
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    secret_kind VARCHAR(32) NOT NULL CHECK (secret_kind IN ('api_key','jwt_token','refresh_token','feed_token')),
    key_version INTEGER NOT NULL CHECK (key_version > 0),
    nonce BYTEA NOT NULL CHECK (octet_length(nonce) = 12),
    ciphertext BYTEA NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (user_id, secret_kind)
);

CREATE INDEX broker_secrets_key_version_idx ON broker_secrets (key_version);

COMMENT ON TABLE broker_secrets IS 'AES-256-GCM encrypted Angel One credentials; keys are supplied only by the application environment.';

-- <<< END: backend/migrations/20260702060000_encrypted_broker_secrets.sql >>>


-- >>> START: backend/migrations/20260702070000_broker_diagnostics.sql <<<
ALTER TABLE user_profiles
    ALTER COLUMN last_token_message TYPE TEXT;

-- <<< END: backend/migrations/20260702070000_broker_diagnostics.sql >>>


-- >>> START: backend/migrations/20260703000000_abuse_prevention.sql <<<
ALTER TABLE users ADD COLUMN IF NOT EXISTS failed_login_attempts INTEGER NOT NULL DEFAULT 0;
ALTER TABLE users ADD COLUMN IF NOT EXISTS locked_until TIMESTAMPTZ;
ALTER TABLE users ADD COLUMN IF NOT EXISTS last_failed_login_at TIMESTAMPTZ;

ALTER TABLE email_otps ADD COLUMN IF NOT EXISTS attempt_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE email_otps ADD COLUMN IF NOT EXISTS invalidated_at TIMESTAMPTZ;
CREATE INDEX IF NOT EXISTS email_otps_active_lookup_idx
    ON email_otps (LOWER(email), purpose, created_at DESC)
    WHERE is_used = FALSE AND invalidated_at IS NULL;

-- <<< END: backend/migrations/20260703000000_abuse_prevention.sql >>>


-- >>> START: backend/migrations/20260703010000_trading_risk_engine.sql <<<
CREATE TABLE IF NOT EXISTS risk_limits (
    user_id UUID REFERENCES users(id) ON DELETE CASCADE,
    max_lots INTEGER CHECK (max_lots IS NULL OR max_lots > 0),
    max_quantity INTEGER CHECK (max_quantity IS NULL OR max_quantity > 0),
    max_notional NUMERIC(20,2) CHECK (max_notional IS NULL OR max_notional > 0),
    max_open_positions INTEGER CHECK (max_open_positions IS NULL OR max_open_positions > 0),
    max_trades_per_day INTEGER CHECK (max_trades_per_day IS NULL OR max_trades_per_day > 0),
    max_daily_realized_loss NUMERIC(20,2) CHECK (max_daily_realized_loss IS NULL OR max_daily_realized_loss > 0),
    max_daily_unrealized_loss NUMERIC(20,2) CHECK (max_daily_unrealized_loss IS NULL OR max_daily_unrealized_loss > 0),
    max_price_age_seconds INTEGER CHECK (max_price_age_seconds IS NULL OR max_price_age_seconds BETWEEN 1 AND 3600),
    margin_requirement_percent NUMERIC(5,2) CHECK (margin_requirement_percent IS NULL OR margin_requirement_percent BETWEEN 0 AND 100),
    updated_by UUID REFERENCES users(id) ON DELETE SET NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE UNIQUE INDEX IF NOT EXISTS risk_limits_global_idx ON risk_limits ((TRUE)) WHERE user_id IS NULL;
CREATE UNIQUE INDEX IF NOT EXISTS risk_limits_user_idx ON risk_limits (user_id) WHERE user_id IS NOT NULL;

INSERT INTO risk_limits (
    user_id,max_lots,max_quantity,max_notional,max_open_positions,max_trades_per_day,
    max_daily_realized_loss,max_daily_unrealized_loss,max_price_age_seconds,margin_requirement_percent
) SELECT NULL,20,10000,100000000,20,100,1000000,1000000,30,10
WHERE NOT EXISTS (SELECT 1 FROM risk_limits WHERE user_id IS NULL);

CREATE TABLE IF NOT EXISTS risk_kill_switches (
    user_id UUID REFERENCES users(id) ON DELETE CASCADE,
    enabled BOOLEAN NOT NULL DEFAULT FALSE,
    reason TEXT NOT NULL DEFAULT '',
    updated_by UUID REFERENCES users(id) ON DELETE SET NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE UNIQUE INDEX IF NOT EXISTS risk_kill_global_idx ON risk_kill_switches ((TRUE)) WHERE user_id IS NULL;
CREATE UNIQUE INDEX IF NOT EXISTS risk_kill_user_idx ON risk_kill_switches (user_id) WHERE user_id IS NOT NULL;
INSERT INTO risk_kill_switches (user_id,enabled,reason)
SELECT NULL,FALSE,'' WHERE NOT EXISTS (SELECT 1 FROM risk_kill_switches WHERE user_id IS NULL);

CREATE TABLE IF NOT EXISTS market_price_ticks (
    contract_token VARCHAR(32) PRIMARY KEY,
    price DOUBLE PRECISION NOT NULL CHECK (price > 0),
    received_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS broker_reconciliation_health (
    user_id UUID PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    healthy BOOLEAN NOT NULL,
    detail TEXT NOT NULL DEFAULT '',
    checked_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS risk_decisions (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    order_id UUID,
    execution_mode VARCHAR(8) NOT NULL CHECK (execution_mode IN ('demo','live')),
    order_role VARCHAR(16) NOT NULL,
    allowed BOOLEAN NOT NULL,
    reason_code VARCHAR(64) NOT NULL,
    message TEXT NOT NULL,
    values JSONB NOT NULL DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS risk_decisions_user_created_idx ON risk_decisions (user_id,created_at DESC);

ALTER TABLE strategy_orders ADD COLUMN IF NOT EXISTS risk_decision_id UUID REFERENCES risk_decisions(id);

-- <<< END: backend/migrations/20260703010000_trading_risk_engine.sql >>>


-- >>> START: backend/migrations/20260703020000_broker_execution_correctness.sql <<<
ALTER TABLE strategy_orders ADD COLUMN IF NOT EXISTS client_order_id VARCHAR(32) NOT NULL DEFAULT '';
ALTER TABLE strategy_orders ADD COLUMN IF NOT EXISTS broker_error_class VARCHAR(24) NOT NULL DEFAULT '';
ALTER TABLE strategy_orders ADD COLUMN IF NOT EXISTS broker_error_code VARCHAR(64) NOT NULL DEFAULT '';
ALTER TABLE strategy_orders ADD COLUMN IF NOT EXISTS submission_attempts INTEGER NOT NULL DEFAULT 0;
ALTER TABLE strategy_orders ADD COLUMN IF NOT EXISTS filled_quantity INTEGER NOT NULL DEFAULT 0;
ALTER TABLE strategy_orders ADD COLUMN IF NOT EXISTS processed_quantity INTEGER NOT NULL DEFAULT 0;
ALTER TABLE strategy_orders ADD COLUMN IF NOT EXISTS average_fill_price DOUBLE PRECISION;
ALTER TABLE strategy_orders ADD COLUMN IF NOT EXISTS last_reconciled_at TIMESTAMPTZ;
ALTER TABLE strategy_orders ADD COLUMN IF NOT EXISTS state_version BIGINT NOT NULL DEFAULT 0;

UPDATE strategy_orders SET client_order_id='RX'||UPPER(SUBSTRING(REPLACE(id::text,'-',''),1,18))
WHERE client_order_id='';
CREATE UNIQUE INDEX IF NOT EXISTS strategy_orders_client_order_id_idx
    ON strategy_orders(client_order_id) WHERE client_order_id<>'';
CREATE INDEX IF NOT EXISTS strategy_orders_live_state_idx
    ON strategy_orders(user_id,status,updated_at)
    WHERE execution_mode='live' AND status IN ('submitting','ambiguous','submitted','partially_filled','processing','cancelling');

CREATE TABLE IF NOT EXISTS broker_order_events (
    id BIGSERIAL PRIMARY KEY,
    order_id UUID NOT NULL REFERENCES strategy_orders(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    from_state VARCHAR(24) NOT NULL,
    to_state VARCHAR(24) NOT NULL,
    event_type VARCHAR(48) NOT NULL,
    broker_order_id VARCHAR(96) NOT NULL DEFAULT '',
    error_class VARCHAR(24) NOT NULL DEFAULT '',
    error_code VARCHAR(64) NOT NULL DEFAULT '',
    diagnostic TEXT NOT NULL DEFAULT '',
    broker_payload JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS broker_order_events_order_idx ON broker_order_events(order_id,id);

-- <<< END: backend/migrations/20260703020000_broker_execution_correctness.sql >>>


-- >>> START: backend/migrations/20260703030000_broker_http_diagnostics.sql <<<
ALTER TABLE strategy_orders ADD COLUMN IF NOT EXISTS broker_http_status INTEGER;
ALTER TABLE broker_order_events ADD COLUMN IF NOT EXISTS http_status INTEGER;

-- <<< END: backend/migrations/20260703030000_broker_http_diagnostics.sql >>>


-- >>> START: backend/migrations/20260709000000_production_operations.sql <<<
CREATE TABLE IF NOT EXISTS audit_events (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    event_type VARCHAR(96) NOT NULL,
    actor_user_id UUID REFERENCES users(id) ON DELETE SET NULL,
    target_user_id UUID REFERENCES users(id) ON DELETE SET NULL,
    request_id VARCHAR(128),
    correlation_id VARCHAR(128),
    ip_address INET,
    summary TEXT NOT NULL DEFAULT '',
    metadata JSONB NOT NULL DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS audit_events_type_created_idx ON audit_events (event_type, created_at DESC);
CREATE INDEX IF NOT EXISTS audit_events_actor_created_idx ON audit_events (actor_user_id, created_at DESC);
CREATE INDEX IF NOT EXISTS audit_events_target_created_idx ON audit_events (target_user_id, created_at DESC);

CREATE OR REPLACE FUNCTION reject_audit_event_mutation()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    RAISE EXCEPTION 'audit_events is append-only';
END;
$$;

DROP TRIGGER IF EXISTS audit_events_no_update ON audit_events;
CREATE TRIGGER audit_events_no_update
BEFORE UPDATE ON audit_events
FOR EACH ROW EXECUTE FUNCTION reject_audit_event_mutation();

DROP TRIGGER IF EXISTS audit_events_no_delete ON audit_events;
CREATE TRIGGER audit_events_no_delete
BEFORE DELETE ON audit_events
FOR EACH ROW EXECUTE FUNCTION reject_audit_event_mutation();

CREATE TABLE IF NOT EXISTS alert_delivery_attempts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    event_type VARCHAR(96) NOT NULL,
    severity VARCHAR(16) NOT NULL,
    channel VARCHAR(32) NOT NULL,
    destination TEXT NOT NULL DEFAULT '',
    status VARCHAR(16) NOT NULL CHECK (status IN ('sent','failed','skipped')),
    error TEXT NOT NULL DEFAULT '',
    payload JSONB NOT NULL DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS alert_delivery_attempts_created_idx
    ON alert_delivery_attempts (created_at DESC);

-- <<< END: backend/migrations/20260709000000_production_operations.sql >>>


-- >>> START: backend/migrations/20260711000000_goldten_demo_balance.sql <<<
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

-- <<< END: backend/migrations/20260711000000_goldten_demo_balance.sql >>>


-- >>> START: backend/migrations/20260713000000_backtesting.sql <<<
ALTER TABLE users ADD COLUMN IF NOT EXISTS can_backtest BOOLEAN NOT NULL DEFAULT FALSE;

CREATE INDEX IF NOT EXISTS users_can_backtest_idx ON users (can_backtest) WHERE can_backtest = TRUE;

COMMENT ON COLUMN users.can_backtest IS 'May run historical strategy backtests and view backtesting pages.';

CREATE TABLE IF NOT EXISTS backtest_market_candles (
    id UUID PRIMARY KEY,
    exchange VARCHAR(16) NOT NULL,
    instrument VARCHAR(32) NOT NULL,
    symbol_token VARCHAR(32) NOT NULL,
    trading_symbol VARCHAR(96) NOT NULL,
    interval_key VARCHAR(32) NOT NULL,
    candle_time TIMESTAMPTZ NOT NULL,
    open_price DOUBLE PRECISION NOT NULL,
    high_price DOUBLE PRECISION NOT NULL,
    low_price DOUBLE PRECISION NOT NULL,
    close_price DOUBLE PRECISION NOT NULL,
    volume DOUBLE PRECISION NOT NULL DEFAULT 0,
    source VARCHAR(32) NOT NULL DEFAULT 'angel_one',
    fetched_by UUID REFERENCES users(id) ON DELETE SET NULL,
    fetched_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (exchange, symbol_token, interval_key, candle_time)
);

CREATE INDEX IF NOT EXISTS backtest_market_candles_lookup_idx
    ON backtest_market_candles (instrument, interval_key, candle_time);

CREATE TABLE IF NOT EXISTS backtest_runs (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    strategy_key VARCHAR(64) NOT NULL,
    instrument VARCHAR(32) NOT NULL,
    trading_symbol VARCHAR(96) NOT NULL,
    symbol_token VARCHAR(32) NOT NULL,
    interval_key VARCHAR(32) NOT NULL,
    lookback_months INTEGER NOT NULL CHECK (lookback_months IN (3, 6)),
    from_time TIMESTAMPTZ NOT NULL,
    to_time TIMESTAMPTZ NOT NULL,
    lots INTEGER NOT NULL CHECK (lots > 0),
    lot_size INTEGER NOT NULL CHECK (lot_size > 0),
    status VARCHAR(16) NOT NULL CHECK (status IN ('completed', 'failed')),
    summary JSONB NOT NULL DEFAULT '{}',
    error TEXT,
    data_points INTEGER NOT NULL DEFAULT 0,
    reused_points INTEGER NOT NULL DEFAULT 0,
    fetched_points INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS backtest_runs_user_idx
    ON backtest_runs (user_id, created_at DESC);
CREATE INDEX IF NOT EXISTS backtest_runs_reuse_idx
    ON backtest_runs (strategy_key, instrument, interval_key, lookback_months, created_at DESC);

CREATE TABLE IF NOT EXISTS backtest_trades (
    id UUID PRIMARY KEY,
    run_id UUID NOT NULL REFERENCES backtest_runs(id) ON DELETE CASCADE,
    trade_date DATE NOT NULL,
    direction VARCHAR(4) NOT NULL CHECK (direction IN ('BUY', 'SELL')),
    entry_time TIMESTAMPTZ NOT NULL,
    entry_price DOUBLE PRECISION NOT NULL,
    exit_time TIMESTAMPTZ NOT NULL,
    exit_price DOUBLE PRECISION NOT NULL,
    lots INTEGER NOT NULL,
    quantity INTEGER NOT NULL,
    realized_pnl DOUBLE PRECISION NOT NULL,
    exit_reason VARCHAR(32) NOT NULL,
    levels JSONB NOT NULL DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS backtest_trades_run_idx
    ON backtest_trades (run_id, entry_time);

-- <<< END: backend/migrations/20260713000000_backtesting.sql >>>


-- >>> START: backend/migrations/20260714000000_margin_estimates.sql <<<
ALTER TABLE backtest_runs DROP CONSTRAINT IF EXISTS backtest_runs_lookback_months_check;
ALTER TABLE backtest_runs
    ADD CONSTRAINT backtest_runs_lookback_months_check
    CHECK (lookback_months IN (1, 3, 6));

CREATE TABLE IF NOT EXISTS broker_margin_estimates (
    id UUID PRIMARY KEY,
    exchange VARCHAR(16) NOT NULL,
    symbol_token VARCHAR(32) NOT NULL,
    trading_symbol VARCHAR(96) NOT NULL,
    product_type VARCHAR(32) NOT NULL,
    trade_type VARCHAR(8) NOT NULL,
    lot_size INTEGER NOT NULL CHECK (lot_size > 0),
    margin_per_lot DOUBLE PRECISION NOT NULL CHECK (margin_per_lot >= 0),
    source VARCHAR(32) NOT NULL DEFAULT 'angel_margin_calculator',
    raw_response JSONB NOT NULL DEFAULT '{}',
    fetched_by UUID REFERENCES users(id) ON DELETE SET NULL,
    fetched_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (exchange, symbol_token, product_type, trade_type, lot_size)
);

CREATE INDEX IF NOT EXISTS broker_margin_estimates_lookup_idx
    ON broker_margin_estimates (exchange, symbol_token, product_type, trade_type, lot_size, fetched_at DESC);

ALTER TABLE strategy_orders
    ADD COLUMN IF NOT EXISTS margin_required DOUBLE PRECISION NOT NULL DEFAULT 0;

ALTER TABLE trades
    ADD COLUMN IF NOT EXISTS margin_required DOUBLE PRECISION NOT NULL DEFAULT 0;

-- <<< END: backend/migrations/20260714000000_margin_estimates.sql >>>


-- >>> START: backend/migrations/20260714010000_margin_estimate_order_type.sql <<<
ALTER TABLE broker_margin_estimates
    ADD COLUMN IF NOT EXISTS order_type VARCHAR(32) NOT NULL DEFAULT 'STOPLOSS_LIMIT';

UPDATE broker_margin_estimates
SET order_type = COALESCE(NULLIF(raw_response->>'order_type', ''), order_type);

ALTER TABLE broker_margin_estimates
    DROP CONSTRAINT IF EXISTS broker_margin_estimates_exchange_symbol_token_product_type_trade_t_key;

CREATE UNIQUE INDEX IF NOT EXISTS broker_margin_estimates_unique_order_type_idx
    ON broker_margin_estimates (exchange, symbol_token, product_type, order_type, trade_type, lot_size);

DROP INDEX IF EXISTS broker_margin_estimates_lookup_idx;
CREATE INDEX IF NOT EXISTS broker_margin_estimates_lookup_idx
    ON broker_margin_estimates (exchange, symbol_token, product_type, order_type, trade_type, lot_size, fetched_at DESC);

-- <<< END: backend/migrations/20260714010000_margin_estimate_order_type.sql >>>


-- >>> START: backend/migrations/20260718010000_correct_goldten_backtest_pnl.sql <<<
-- Futures Breakout v3 originally treated one configured GOLDTEN lot as one
-- price unit. Correct saved results to use the broker contract lot size.
UPDATE backtest_trades AS trade
SET quantity = trade.lots * run.lot_size,
    realized_pnl = trade.realized_pnl * run.lot_size
FROM backtest_runs AS run
WHERE trade.run_id = run.id
  AND run.strategy_key = 'futures_breakout_v3'
  AND run.instrument = 'GOLDTEN'
  AND run.lot_size <> 1
  AND COALESCE((run.summary ->> 'pnl_multiplier_per_lot')::DOUBLE PRECISION, 1) = 1;

UPDATE backtest_runs
SET summary = summary || jsonb_build_object(
    'net_pnl', COALESCE((summary ->> 'net_pnl')::DOUBLE PRECISION, 0) * lot_size,
    'gross_profit', COALESCE((summary ->> 'gross_profit')::DOUBLE PRECISION, 0) * lot_size,
    'gross_loss', COALESCE((summary ->> 'gross_loss')::DOUBLE PRECISION, 0) * lot_size,
    'average_pnl', COALESCE((summary ->> 'average_pnl')::DOUBLE PRECISION, 0) * lot_size,
    'average_win', COALESCE((summary ->> 'average_win')::DOUBLE PRECISION, 0) * lot_size,
    'average_loss', COALESCE((summary ->> 'average_loss')::DOUBLE PRECISION, 0) * lot_size,
    'max_drawdown', COALESCE((summary ->> 'max_drawdown')::DOUBLE PRECISION, 0) * lot_size,
    'pnl_multiplier_per_lot', lot_size
)
WHERE strategy_key = 'futures_breakout_v3'
  AND instrument = 'GOLDTEN'
  AND lot_size <> 1
  AND COALESCE((summary ->> 'pnl_multiplier_per_lot')::DOUBLE PRECISION, 1) = 1;

-- <<< END: backend/migrations/20260718010000_correct_goldten_backtest_pnl.sql >>>


-- >>> START: backend/migrations/20260724000000_broker_credential_revision.sql <<<
ALTER TABLE user_profiles
    ADD COLUMN IF NOT EXISTS broker_credential_revision BIGINT NOT NULL DEFAULT 0;

-- <<< END: backend/migrations/20260724000000_broker_credential_revision.sql >>>


-- >>> START: backend/migrations/20260724010000_market_price_ticks_exchange_segment.sql <<<
ALTER TABLE market_price_ticks
    ADD COLUMN IF NOT EXISTS exchange_segment VARCHAR(8) NOT NULL DEFAULT 'MCX';

UPDATE market_price_ticks
SET exchange_segment = 'MCX'
WHERE exchange_segment = '';

DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conname = 'market_price_ticks_pkey'
          AND conrelid = 'market_price_ticks'::regclass
          AND pg_get_constraintdef(oid) = 'PRIMARY KEY (contract_token)'
    ) THEN
        ALTER TABLE market_price_ticks DROP CONSTRAINT market_price_ticks_pkey;
    END IF;

    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conname = 'market_price_ticks_pkey'
          AND conrelid = 'market_price_ticks'::regclass
    ) THEN
        ALTER TABLE market_price_ticks
            ADD CONSTRAINT market_price_ticks_pkey PRIMARY KEY (exchange_segment, contract_token);
    END IF;
END $$;

-- <<< END: backend/migrations/20260724010000_market_price_ticks_exchange_segment.sql >>>


-- >>> START: backend/migrations/20260724020000_correct_goldten_demo_trade_pnl.sql <<<
CREATE TEMP TABLE goldten_demo_pnl_fix AS
WITH exit_fills AS (
    SELECT
        t.id AS trade_id,
        SUM(
            (CASE WHEN t.direction = 'BUY'
                  THEN o.filled_price::numeric - t.entry_price
                  ELSE t.entry_price - o.filled_price::numeric
             END) * (o.processed_quantity::numeric / s.lot_size::numeric)
        ) AS exit_pnl
    FROM trades t
    JOIN strategy_market_snapshots s ON s.id = t.strategy_snapshot_id
    JOIN strategy_orders o ON o.trade_id = t.id
    WHERE t.execution_mode = 'demo'
      AND t.status = 'closed'
      AND t.strategy_key = 'futures_breakout_v3'
      AND t.instrument_label = 'GOLDTEN'
      AND s.lot_size IS NOT NULL
      AND s.lot_size > 1
      AND o.role IN ('TARGET', 'SL1', 'SL2')
      AND o.status = 'filled'
      AND o.processed_quantity > 0
      AND o.filled_price IS NOT NULL
    GROUP BY t.id
),
candidates AS (
    SELECT
        t.id,
        t.user_id,
        t.pnl::numeric(18, 2) AS old_pnl,
        ROUND(COALESCE(
            f.exit_pnl,
            (CASE WHEN t.direction = 'BUY'
                  THEN t.exit_price - t.entry_price
                  ELSE t.entry_price - t.exit_price
             END) * GREATEST(
                 COALESCE(NULLIF(t.total_lots, 0), 0),
                 CASE
                     WHEN t.quantity > 0 THEN CEIL(t.quantity::numeric / s.lot_size::numeric)::int
                     ELSE 0
                 END
             )::numeric
        )::numeric, 2) AS corrected_pnl,
        s.lot_size
    FROM trades t
    JOIN strategy_market_snapshots s ON s.id = t.strategy_snapshot_id
    LEFT JOIN exit_fills f ON f.trade_id = t.id
    WHERE t.execution_mode = 'demo'
      AND t.status = 'closed'
      AND t.strategy_key = 'futures_breakout_v3'
      AND t.instrument_label = 'GOLDTEN'
      AND s.lot_size IS NOT NULL
      AND s.lot_size > 1
      AND t.entry_price IS NOT NULL
      AND t.exit_price IS NOT NULL
)
SELECT *, corrected_pnl - old_pnl AS pnl_delta
FROM candidates
WHERE ABS(old_pnl - corrected_pnl * lot_size) < 0.05
  AND ABS(old_pnl - corrected_pnl) > 0.05;

UPDATE trades t
SET pnl = f.corrected_pnl,
    updated_at = NOW(),
    notes = CASE
        WHEN t.notes LIKE '%GOLDTEN demo P&L lot-size correction%'
            THEN t.notes
        ELSE CONCAT(t.notes, '; GOLDTEN demo P&L lot-size correction 2026-07-24')
    END
FROM goldten_demo_pnl_fix f
WHERE t.id = f.id;

UPDATE user_profiles p
SET demo_balance = (p.demo_balance + totals.balance_delta)::numeric(18, 2),
    updated_at = NOW()
FROM (
    SELECT user_id, SUM(pnl_delta)::numeric(18, 2) AS balance_delta
    FROM goldten_demo_pnl_fix
    GROUP BY user_id
) totals
WHERE p.user_id = totals.user_id;

-- <<< END: backend/migrations/20260724020000_correct_goldten_demo_trade_pnl.sql >>>


-- >>> START: backend/migrations/20260724025000_strategy_snapshot_execution_metadata.sql <<<
-- This migration intentionally sorts immediately before the historical
-- option-entry index migration.  Clean databases otherwise reach that index
-- before these execution metadata columns exist.  The statements are
-- idempotent so an existing installation that already has the columns can
-- safely apply this previously missing migration without rewriting data.
ALTER TABLE strategy_market_snapshots
    ADD COLUMN IF NOT EXISTS exchange_segment VARCHAR(16) NOT NULL DEFAULT 'MCX',
    ADD COLUMN IF NOT EXISTS product_type VARCHAR(32) NOT NULL DEFAULT 'CARRYFORWARD',
    ADD COLUMN IF NOT EXISTS execution_key VARCHAR(96) NOT NULL DEFAULT 'default',
    ADD COLUMN IF NOT EXISTS underlying_token VARCHAR(32) NOT NULL DEFAULT '';

UPDATE strategy_market_snapshots
SET execution_key = 'default'
WHERE execution_key = '';

-- <<< END: backend/migrations/20260724025000_strategy_snapshot_execution_metadata.sql >>>


-- >>> START: backend/migrations/20260724030000_option_entry_strategy_v1.sql <<<
ALTER TABLE strategy_market_snapshots
    DROP CONSTRAINT IF EXISTS strategy_market_snapshots_strategy_key_instrument_trade_dat_key;

CREATE UNIQUE INDEX IF NOT EXISTS strategy_market_snapshots_execution_idx
    ON strategy_market_snapshots (strategy_key, instrument, trade_date, execution_key);

-- <<< END: backend/migrations/20260724030000_option_entry_strategy_v1.sql >>>


-- >>> START: backend/migrations/20260725000000_correct_goldten_backtest_pnl_multiplier.sql <<<
CREATE TEMP TABLE goldten_backtest_pnl_fix AS
SELECT id AS run_id,
       lot_size::DOUBLE PRECISION AS multiplier
FROM backtest_runs
WHERE instrument = 'GOLDTEN'
  AND lot_size > 1
  AND COALESCE((summary ->> 'pnl_multiplier_per_lot')::DOUBLE PRECISION, 1) = lot_size::DOUBLE PRECISION;

UPDATE backtest_trades trade
SET realized_pnl = trade.realized_pnl / fix.multiplier,
    levels = trade.levels || jsonb_build_object(
        'partial_realized_pnl',
        COALESCE((trade.levels ->> 'partial_realized_pnl')::DOUBLE PRECISION, 0) / fix.multiplier,
        'final_leg_pnl',
        COALESCE((trade.levels ->> 'final_leg_pnl')::DOUBLE PRECISION, 0) / fix.multiplier,
        'calculated_pnl',
        COALESCE((trade.levels ->> 'calculated_pnl')::DOUBLE PRECISION, trade.realized_pnl) / fix.multiplier,
        'pnl_model',
        'goldten_points_x_lots'
    )
FROM goldten_backtest_pnl_fix fix
WHERE trade.run_id = fix.run_id;

UPDATE backtest_runs run
SET summary = run.summary || jsonb_build_object(
        'net_pnl',
        COALESCE((run.summary ->> 'net_pnl')::DOUBLE PRECISION, 0) / fix.multiplier,
        'gross_profit',
        COALESCE((run.summary ->> 'gross_profit')::DOUBLE PRECISION, 0) / fix.multiplier,
        'gross_loss',
        COALESCE((run.summary ->> 'gross_loss')::DOUBLE PRECISION, 0) / fix.multiplier,
        'average_pnl',
        COALESCE((run.summary ->> 'average_pnl')::DOUBLE PRECISION, 0) / fix.multiplier,
        'average_win',
        COALESCE((run.summary ->> 'average_win')::DOUBLE PRECISION, 0) / fix.multiplier,
        'average_loss',
        COALESCE((run.summary ->> 'average_loss')::DOUBLE PRECISION, 0) / fix.multiplier,
        'max_drawdown',
        COALESCE((run.summary ->> 'max_drawdown')::DOUBLE PRECISION, 0) / fix.multiplier,
        'pnl_multiplier_per_lot',
        1,
        'pnl_model',
        'goldten_points_x_lots'
    )
FROM goldten_backtest_pnl_fix fix
WHERE run.id = fix.run_id;

-- <<< END: backend/migrations/20260725000000_correct_goldten_backtest_pnl_multiplier.sql >>>


-- >>> START: backend/migrations/20260725010000_sl2_reversal_intents.sql <<<
CREATE TABLE IF NOT EXISTS strategy_reversal_intents (
    source_trade_id UUID PRIMARY KEY REFERENCES trades(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    snapshot_id UUID NOT NULL REFERENCES strategy_market_snapshots(id),
    instrument VARCHAR(32) NOT NULL,
    source_direction VARCHAR(4) NOT NULL CHECK (source_direction IN ('BUY', 'SELL')),
    reversal_direction VARCHAR(4) NOT NULL CHECK (reversal_direction IN ('BUY', 'SELL')),
    lots INTEGER NOT NULL CHECK (lots > 0),
    entry_price DOUBLE PRECISION NOT NULL CHECK (entry_price > 0),
    order_session_key VARCHAR(32) NOT NULL UNIQUE,
    status VARCHAR(24) NOT NULL DEFAULT 'pending'
        CHECK (status IN (
            'pending',
            'processing',
            'waiting',
            'submitted',
            'completed',
            'failed',
            'cancelled'
        )),
    attempts INTEGER NOT NULL DEFAULT 0,
    next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_error TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS strategy_reversal_intents_due_idx
    ON strategy_reversal_intents (status, next_attempt_at);

-- <<< END: backend/migrations/20260725010000_sl2_reversal_intents.sql >>>


-- >>> START: backend/migrations/20260726000000_preserve_futures_breakout_tp1.sql <<<
WITH original_targets AS (
    SELECT DISTINCT ON (orders.trade_id)
        orders.trade_id,
        orders.price
    FROM strategy_orders orders
    JOIN trades trade ON trade.id = orders.trade_id
    WHERE trade.strategy_key = 'futures_breakout_v3'
      AND trade.status = 'open'
      AND orders.role = 'TARGET'
      AND orders.price > 0
    ORDER BY orders.trade_id, orders.created_at
)
UPDATE trades trade
SET target_price = original_targets.price,
    updated_at = NOW()
FROM original_targets
WHERE trade.id = original_targets.trade_id
  AND trade.strategy_key = 'futures_breakout_v3'
  AND trade.status = 'open';

-- <<< END: backend/migrations/20260726000000_preserve_futures_breakout_tp1.sql >>>


-- >>> START: backend/migrations/20260726010000_reversal_trades_use_fresh_levels.sql <<<
ALTER TABLE trades
    ADD COLUMN IF NOT EXISTS reversal_of_trade_id UUID
    REFERENCES trades(id) ON DELETE SET NULL;

CREATE INDEX IF NOT EXISTS trades_reversal_of_idx
    ON trades (reversal_of_trade_id)
    WHERE reversal_of_trade_id IS NOT NULL;

UPDATE trades AS reversal
SET reversal_of_trade_id = intent.source_trade_id,
    updated_at = NOW()
FROM strategy_orders AS entry_order
JOIN strategy_reversal_intents AS intent
  ON intent.user_id = entry_order.user_id
 AND intent.order_session_key = entry_order.session_key
WHERE entry_order.trade_id = reversal.id
  AND entry_order.role IN ('BUY_ENTRY', 'SELL_ENTRY')
  AND reversal.strategy_key = 'futures_breakout_v3'
  AND reversal.reversal_of_trade_id IS NULL;

UPDATE trades AS reversal
SET target_price = CASE
        WHEN EXISTS (
            SELECT 1
            FROM strategy_orders AS target_order
            WHERE target_order.trade_id = reversal.id
              AND target_order.role = 'TARGET'
              AND target_order.processed_quantity > 0
        ) THEN reversal.target_price
        WHEN reversal.direction = 'BUY' THEN reversal.entry_price::float8 * (1.0 + 0.015)
        ELSE reversal.entry_price::float8 * (1.0 - 0.015)
    END,
    sl1_price = CASE
        WHEN reversal.direction = 'BUY' THEN CASE
            WHEN snapshot.ll2 * (1.0 - 0.0012) < reversal.entry_price::float8
                THEN GREATEST(
                    reversal.entry_price::float8 * (1.0 - 0.015),
                    snapshot.ll2 * (1.0 - 0.0012)
                )
            ELSE reversal.entry_price::float8 * (1.0 - 0.015)
        END
        ELSE CASE
            WHEN snapshot.hh2 * (1.0 + 0.0012) > reversal.entry_price::float8
                THEN LEAST(
                    reversal.entry_price::float8 * (1.0 + 0.015),
                    snapshot.hh2 * (1.0 + 0.0012)
                )
            ELSE reversal.entry_price::float8 * (1.0 + 0.015)
        END
    END,
    sl2_price = CASE
        WHEN reversal.direction = 'BUY' THEN CASE
            WHEN snapshot.ll4 * (1.0 - 0.0012) < reversal.entry_price::float8
                THEN GREATEST(
                    reversal.entry_price::float8 * (1.0 - 0.015),
                    snapshot.ll4 * (1.0 - 0.0012)
                )
            ELSE reversal.entry_price::float8 * (1.0 - 0.015)
        END
        ELSE CASE
            WHEN snapshot.hh4 * (1.0 + 0.0012) > reversal.entry_price::float8
                THEN LEAST(
                    reversal.entry_price::float8 * (1.0 + 0.015),
                    snapshot.hh4 * (1.0 + 0.0012)
                )
            ELSE reversal.entry_price::float8 * (1.0 + 0.015)
        END
    END,
    updated_at = NOW()
FROM strategy_market_snapshots AS snapshot
WHERE reversal.strategy_snapshot_id = snapshot.id
  AND reversal.strategy_key = 'futures_breakout_v3'
  AND reversal.status = 'open'
  AND reversal.reversal_of_trade_id IS NOT NULL
  AND reversal.direction IN ('BUY', 'SELL')
  AND snapshot.hh2 IS NOT NULL
  AND snapshot.ll2 IS NOT NULL
  AND snapshot.hh4 IS NOT NULL
  AND snapshot.ll4 IS NOT NULL;

-- <<< END: backend/migrations/20260726010000_reversal_trades_use_fresh_levels.sql >>>


-- >>> START: backend/migrations/20260728000000_futures_gap_entry.sql <<<
ALTER TABLE strategy_market_snapshots
    ADD COLUMN IF NOT EXISTS previous_close DOUBLE PRECISION,
    ADD COLUMN IF NOT EXISTS market_open DOUBLE PRECISION,
    ADD COLUMN IF NOT EXISTS gap_direction VARCHAR(8),
    ADD COLUMN IF NOT EXISTS entry_direction VARCHAR(8),
    ADD COLUMN IF NOT EXISTS entry_source VARCHAR(24),
    ADD COLUMN IF NOT EXISTS gap_plan_status VARCHAR(24),
    ADD COLUMN IF NOT EXISTS opening_range_high DOUBLE PRECISION,
    ADD COLUMN IF NOT EXISTS opening_range_low DOUBLE PRECISION,
    ADD COLUMN IF NOT EXISTS planned_entry DOUBLE PRECISION,
    ADD COLUMN IF NOT EXISTS planned_target DOUBLE PRECISION,
    ADD COLUMN IF NOT EXISTS planned_sl1 DOUBLE PRECISION,
    ADD COLUMN IF NOT EXISTS planned_sl2 DOUBLE PRECISION,
    ADD COLUMN IF NOT EXISTS gap_planned_at TIMESTAMPTZ;

CREATE INDEX IF NOT EXISTS strategy_snapshots_gap_plan_idx
    ON strategy_market_snapshots (strategy_key, trade_date, gap_plan_status);

-- <<< END: backend/migrations/20260728000000_futures_gap_entry.sql >>>


-- >>> START: backend/migrations/20260728010000_futures_gap_scheduler_actions.sql <<<
ALTER TABLE strategy_scheduler_runs
    DROP CONSTRAINT IF EXISTS strategy_scheduler_runs_action_check;

ALTER TABLE strategy_scheduler_runs
    ADD CONSTRAINT strategy_scheduler_runs_action_check
    CHECK (action IN ('target', 'stop', 'entry', 'gap_entry'));

-- <<< END: backend/migrations/20260728010000_futures_gap_scheduler_actions.sql >>>


-- >>> START: backend/migrations/20260728020000_cancel_legacy_gap_demo_entries.sql <<<
UPDATE strategy_orders AS orders
SET status = 'cancelled',
    broker_status = 'Cancelled during Futures Breakout gap-entry rule upgrade',
    updated_at = NOW()
FROM strategy_market_snapshots AS snapshot
WHERE snapshot.id = orders.snapshot_id
  AND snapshot.strategy_key = 'futures_breakout_v3'
  AND snapshot.gap_plan_status IS NULL
  AND orders.execution_mode = 'demo'
  AND orders.role IN ('BUY_ENTRY', 'SELL_ENTRY')
  AND orders.status IN (
      'pending',
      'submitting',
      'ambiguous',
      'submitted',
      'partially_filled',
      'processing',
      'cancelling'
  );

-- <<< END: backend/migrations/20260728020000_cancel_legacy_gap_demo_entries.sql >>>


-- >>> START: backend/migrations/20260728030000_update_futures_breakout_instruments.sql <<<
DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM trades
        WHERE strategy_key = 'futures_breakout_v3'
          AND instrument_label = 'GOLD'
          AND status = 'open'
    ) THEN
        RAISE EXCEPTION
            'Cannot remove GOLD while a Futures Breakout position is open';
    END IF;

    IF EXISTS (
        SELECT 1
        FROM strategy_orders AS orders
        JOIN strategy_market_snapshots AS snapshot
          ON snapshot.id = orders.snapshot_id
        WHERE snapshot.strategy_key = 'futures_breakout_v3'
          AND snapshot.instrument = 'GOLD'
          AND orders.execution_mode = 'live'
          AND orders.status IN (
              'pending',
              'submitting',
              'ambiguous',
              'submitted',
              'partially_filled',
              'processing',
              'cancelling'
          )
    ) THEN
        RAISE EXCEPTION
            'Cannot remove GOLD while a live Futures Breakout order is active';
    END IF;
END
$$;

UPDATE strategy_orders AS orders
SET status = 'cancelled',
    broker_status = 'Cancelled because GOLD was removed from Futures Breakout',
    updated_at = NOW()
FROM strategy_market_snapshots AS snapshot
WHERE snapshot.id = orders.snapshot_id
  AND snapshot.strategy_key = 'futures_breakout_v3'
  AND snapshot.instrument = 'GOLD'
  AND orders.execution_mode = 'demo'
  AND orders.status IN (
      'pending',
      'submitting',
      'ambiguous',
      'submitted',
      'partially_filled',
      'processing',
      'cancelling'
  );

DELETE FROM user_strategy_configs
WHERE strategy_key = 'futures_breakout_v3'
  AND instrument = 'GOLD';

-- <<< END: backend/migrations/20260728030000_update_futures_breakout_instruments.sql >>>


-- >>> START: backend/migrations/20260728110000_backtest_trading_day_permission.sql <<<
ALTER TABLE users
    ADD COLUMN IF NOT EXISTS can_backtest_on_trading_days BOOLEAN NOT NULL DEFAULT FALSE;

ALTER TABLE users
    ADD CONSTRAINT users_trading_day_backtest_requires_backtest
    CHECK (NOT can_backtest_on_trading_days OR can_backtest);

COMMENT ON COLUMN users.can_backtest_on_trading_days IS
    'May run backtests during an Indian trading day; requires can_backtest.';

-- <<< END: backend/migrations/20260728110000_backtest_trading_day_permission.sql >>>


-- >>> START: backend/migrations/20260729000000_backtest_option_contract_snapshots.sql <<<
CREATE TABLE IF NOT EXISTS backtest_option_contracts (
    id UUID PRIMARY KEY,
    snapshot_date DATE NOT NULL,
    instrument VARCHAR(32) NOT NULL,
    side VARCHAR(2) NOT NULL CHECK (side IN ('CE', 'PE')),
    exchange VARCHAR(16) NOT NULL,
    symbol_token VARCHAR(32) NOT NULL,
    trading_symbol VARCHAR(96) NOT NULL,
    expiry_date DATE NOT NULL,
    strike_price DOUBLE PRECISION NOT NULL,
    lot_size INTEGER NOT NULL CHECK (lot_size > 0),
    source VARCHAR(32) NOT NULL DEFAULT 'angel_one_master',
    fetched_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (snapshot_date, instrument, side, symbol_token)
);

CREATE INDEX IF NOT EXISTS backtest_option_contracts_lookup_idx
    ON backtest_option_contracts (instrument, side, expiry_date, strike_price);

CREATE INDEX IF NOT EXISTS backtest_option_contracts_snapshot_idx
    ON backtest_option_contracts (instrument, snapshot_date DESC);

COMMENT ON TABLE backtest_option_contracts IS
    'Daily option contract snapshots retained for historically valid option-entry backtests.';

-- <<< END: backend/migrations/20260729000000_backtest_option_contract_snapshots.sql >>>


-- >>> START: backend/migrations/20260809000000_supertrend_index_options.sql <<<
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

-- <<< END: backend/migrations/20260809000000_supertrend_index_options.sql >>>


-- >>> START: backend/migrations/20260812000000_trade_exit_audit.sql <<<
ALTER TABLE trades
    ADD COLUMN IF NOT EXISTS exit_reason VARCHAR(32) NOT NULL DEFAULT '',
    ADD COLUMN IF NOT EXISTS tp1_exit_price NUMERIC(18,2),
    ADD COLUMN IF NOT EXISTS tp1_exit_datetime TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS tp1_exit_quantity INTEGER NOT NULL DEFAULT 0;

WITH target_fills AS (
    SELECT DISTINCT ON (trade_id)
        trade_id,
        COALESCE(average_fill_price, price)::numeric(18,2) AS price,
        updated_at,
        processed_quantity
    FROM strategy_orders
    WHERE trade_id IS NOT NULL
      AND role='TARGET'
      AND processed_quantity>0
    ORDER BY trade_id,updated_at DESC,id DESC
)
UPDATE trades AS trade
SET tp1_exit_price=target.price,
    tp1_exit_datetime=target.updated_at,
    tp1_exit_quantity=target.processed_quantity
FROM target_fills AS target
WHERE trade.id=target.trade_id
  AND trade.tp1_exit_price IS NULL;

WITH final_fills AS (
    SELECT DISTINCT ON (trade_id)
        trade_id,
        role,
        session_key
    FROM strategy_orders
    WHERE trade_id IS NOT NULL
      AND role IN ('TARGET','SL1','SL2')
      AND processed_quantity>0
    ORDER BY trade_id,updated_at DESC,id DESC
)
UPDATE trades AS trade
SET exit_reason=CASE
        WHEN final.session_key LIKE 'optsq-%' OR final.session_key LIKE 'stsq-%'
            THEN 'MARKET_CLOSED'
        WHEN final.session_key LIKE 'strev-%'
            THEN 'SIGNAL_REVERSAL'
        WHEN trade.strategy_key='futures_breakout_v3' AND final.role='TARGET'
            THEN 'TP1'
        WHEN trade.strategy_key='futures_breakout_v3'
            THEN final.role
        WHEN final.role='TARGET' THEN 'TP'
        ELSE 'SL'
    END
FROM final_fills AS final
WHERE trade.id=final.trade_id
  AND trade.status='closed'
  AND trade.exit_reason='';

UPDATE trades AS trade
SET quantity=trade.total_lots*snapshot.lot_size
FROM strategy_market_snapshots AS snapshot
WHERE trade.strategy_snapshot_id=snapshot.id
  AND trade.strategy_key='futures_breakout_v3'
  AND trade.status='closed'
  AND trade.quantity=0
  AND trade.total_lots>0
  AND snapshot.lot_size>0;

-- <<< END: backend/migrations/20260812000000_trade_exit_audit.sql >>>


-- >>> START: backend/migrations/20260812010000_correct_trade_exit_audit_timestamps.sql <<<
WITH target_fills AS (
    SELECT DISTINCT ON (trade_id)
        trade_id,
        COALESCE(average_fill_price, price)::numeric(18,2) AS price,
        COALESCE(filled_at, updated_at) AS filled_at,
        processed_quantity
    FROM strategy_orders
    WHERE trade_id IS NOT NULL
      AND role = 'TARGET'
      AND processed_quantity > 0
    ORDER BY trade_id, COALESCE(filled_at, updated_at) DESC, id DESC
)
UPDATE trades AS trade
SET tp1_exit_price = target.price,
    tp1_exit_datetime = target.filled_at,
    tp1_exit_quantity = target.processed_quantity
FROM target_fills AS target
WHERE trade.id = target.trade_id;

WITH final_fills AS (
    SELECT DISTINCT ON (trade_id)
        trade_id,
        role,
        session_key
    FROM strategy_orders
    WHERE trade_id IS NOT NULL
      AND role IN ('TARGET', 'SL1', 'SL2')
      AND processed_quantity > 0
    ORDER BY trade_id, COALESCE(filled_at, updated_at) DESC, id DESC
)
UPDATE trades AS trade
SET exit_reason = CASE
        WHEN final.session_key LIKE 'optsq-%' OR final.session_key LIKE 'stsq-%'
            THEN 'MARKET_CLOSED'
        WHEN final.session_key LIKE 'strev-%'
            THEN 'SIGNAL_REVERSAL'
        WHEN trade.strategy_key = 'futures_breakout_v3' AND final.role = 'TARGET'
            THEN 'TP1'
        WHEN trade.strategy_key = 'futures_breakout_v3'
            THEN final.role
        WHEN final.role = 'TARGET' THEN 'TP'
        ELSE 'SL'
    END
FROM final_fills AS final
WHERE trade.id = final.trade_id
  AND trade.status = 'closed';

-- <<< END: backend/migrations/20260812010000_correct_trade_exit_audit_timestamps.sql >>>


-- >>> START: backend/migrations/20260816000000_fix_margin_estimate_unique_constraint.sql <<<
WITH ranked AS (
    SELECT
        id,
        ROW_NUMBER() OVER (
            PARTITION BY exchange, symbol_token, product_type, order_type, trade_type, lot_size
            ORDER BY fetched_at DESC, id DESC
        ) AS row_number
    FROM broker_margin_estimates
)
DELETE FROM broker_margin_estimates estimates
USING ranked
WHERE estimates.id = ranked.id
  AND ranked.row_number > 1;

ALTER TABLE broker_margin_estimates
    DROP CONSTRAINT IF EXISTS broker_margin_estimates_exchange_symbol_token_product_type_trade_t_key;

ALTER TABLE broker_margin_estimates
    DROP CONSTRAINT IF EXISTS broker_margin_estimates_exchange_symbol_token_product_type__key;

DO $$
DECLARE
    old_constraint record;
BEGIN
    FOR old_constraint IN
        SELECT constraint_name
        FROM information_schema.table_constraints
        WHERE table_schema = 'public'
          AND table_name = 'broker_margin_estimates'
          AND constraint_type = 'UNIQUE'
          AND constraint_name <> 'broker_margin_estimates_unique_order_type_idx'
    LOOP
        IF (
            SELECT array_agg(column_name::text ORDER BY ordinal_position)
            FROM information_schema.key_column_usage
            WHERE table_schema = 'public'
              AND table_name = 'broker_margin_estimates'
              AND constraint_name = old_constraint.constraint_name
        ) = ARRAY['exchange','symbol_token','product_type','trade_type','lot_size'] THEN
            EXECUTE format(
                'ALTER TABLE broker_margin_estimates DROP CONSTRAINT IF EXISTS %I',
                old_constraint.constraint_name
            );
        END IF;
    END LOOP;
END $$;

CREATE UNIQUE INDEX IF NOT EXISTS broker_margin_estimates_unique_order_type_idx
    ON broker_margin_estimates (exchange, symbol_token, product_type, order_type, trade_type, lot_size);

-- <<< END: backend/migrations/20260816000000_fix_margin_estimate_unique_constraint.sql >>>


-- >>> START: backend/migrations/20260819000000_durable_signal_fanout.sql <<<
CREATE TABLE IF NOT EXISTS strategy_signals (
    id UUID PRIMARY KEY,
    strategy_key VARCHAR(64) NOT NULL,
    instrument VARCHAR(32) NOT NULL,
    session_key VARCHAR(96) NOT NULL,
    signal_at TIMESTAMPTZ NOT NULL,
    snapshot_id UUID REFERENCES strategy_market_snapshots(id) ON DELETE SET NULL,
    signal_type VARCHAR(32) NOT NULL,
    status VARCHAR(24) NOT NULL DEFAULT 'confirmed'
        CHECK (status IN ('confirmed', 'dispatching', 'completed', 'partial', 'failed', 'expired')),
    expected_users INTEGER NOT NULL DEFAULT 0 CHECK (expected_users >= 0),
    payload JSONB NOT NULL DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (strategy_key, instrument, session_key, signal_type)
);

CREATE INDEX IF NOT EXISTS strategy_signals_day_idx
    ON strategy_signals (signal_at DESC, strategy_key, instrument);

CREATE TABLE IF NOT EXISTS strategy_execution_intents (
    id UUID PRIMARY KEY,
    signal_id UUID NOT NULL REFERENCES strategy_signals(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    snapshot_id UUID REFERENCES strategy_market_snapshots(id) ON DELETE SET NULL,
    trade_id UUID REFERENCES trades(id) ON DELETE SET NULL,
    strategy_key VARCHAR(64) NOT NULL,
    instrument VARCHAR(32) NOT NULL,
    session_key VARCHAR(96) NOT NULL,
    action VARCHAR(32) NOT NULL,
    role VARCHAR(16) NOT NULL,
    side VARCHAR(4) NOT NULL CHECK (side IN ('BUY', 'SELL')),
    order_type VARCHAR(24) NOT NULL,
    lots INTEGER NOT NULL CHECK (lots > 0),
    quantity INTEGER,
    price DOUBLE PRECISION NOT NULL,
    trigger_price DOUBLE PRECISION,
    status VARCHAR(24) NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'claimed', 'retry_wait', 'submitted', 'completed', 'skipped', 'failed', 'expired')),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at TIMESTAMPTZ,
    last_error TEXT NOT NULL DEFAULT '',
    strategy_order_id UUID REFERENCES strategy_orders(id) ON DELETE SET NULL,
    claimed_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE UNIQUE INDEX IF NOT EXISTS strategy_entry_intents_signal_user_role_idx
    ON strategy_execution_intents (signal_id, user_id, action, role)
    WHERE trade_id IS NULL;

CREATE INDEX IF NOT EXISTS strategy_execution_intents_due_idx
    ON strategy_execution_intents (status, next_attempt_at, created_at)
    WHERE status IN ('pending', 'retry_wait');

CREATE INDEX IF NOT EXISTS strategy_execution_intents_user_day_idx
    ON strategy_execution_intents (user_id, created_at DESC);

CREATE UNIQUE INDEX IF NOT EXISTS strategy_square_off_intents_trade_idx
    ON strategy_execution_intents (trade_id, action)
    WHERE trade_id IS NOT NULL AND action = 'SQUARE_OFF';

-- <<< END: backend/migrations/20260819000000_durable_signal_fanout.sql >>>


-- >>> START: backend/migrations/20260823000000_remove_margin_and_option_entry.sql <<<
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

-- <<< END: backend/migrations/20260823000000_remove_margin_and_option_entry.sql >>>


-- >>> START: backend/migrations/20260823010000_execution_safety_lifecycle.sql <<<
ALTER TABLE trades
    ADD COLUMN IF NOT EXISTS safety_status VARCHAR(32) NOT NULL DEFAULT 'DEMO',
    ADD COLUMN IF NOT EXISTS protection_deadline_at TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS protection_attempts INTEGER NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS last_protection_error TEXT NOT NULL DEFAULT '',
    ADD COLUMN IF NOT EXISTS broker_net_quantity INTEGER,
    ADD COLUMN IF NOT EXISTS broker_average_price DOUBLE PRECISION,
    ADD COLUMN IF NOT EXISTS last_position_reconciled_at TIMESTAMPTZ;

ALTER TABLE strategy_orders
    ADD COLUMN IF NOT EXISTS uncertain_since_at TIMESTAMPTZ;

UPDATE trades
SET safety_status = CASE
    WHEN status = 'closed' THEN 'CLOSED'
    WHEN execution_mode = 'demo' THEN 'DEMO'
    ELSE 'PROTECTION_REQUIRED'
END
WHERE safety_status = 'DEMO';

ALTER TABLE trades DROP CONSTRAINT IF EXISTS trades_safety_status_check;
ALTER TABLE trades ADD CONSTRAINT trades_safety_status_check CHECK (safety_status IN (
    'DEMO',
    'PROTECTION_REQUIRED',
    'PROTECTION_SUBMITTING',
    'PROTECTION_UNCERTAIN',
    'PROTECTED',
    'PROTECTION_FAILED',
    'CLOSING',
    'EMERGENCY_CLOSING',
    'RECONCILIATION_REQUIRED',
    'CLOSED'
));

ALTER TABLE trades DROP CONSTRAINT IF EXISTS trades_protection_attempts_check;
ALTER TABLE trades ADD CONSTRAINT trades_protection_attempts_check
    CHECK (protection_attempts >= 0);

CREATE INDEX IF NOT EXISTS trades_execution_safety_idx
    ON trades (execution_mode, safety_status, updated_at)
    WHERE status = 'open';

DO $$
DECLARE
    constraint_name TEXT;
BEGIN
    FOR constraint_name IN
        SELECT conname
        FROM pg_constraint
        WHERE conrelid = 'strategy_orders'::regclass
          AND contype = 'c'
          AND pg_get_constraintdef(oid) LIKE '%role%BUY_ENTRY%'
    LOOP
        EXECUTE format('ALTER TABLE strategy_orders DROP CONSTRAINT %I', constraint_name);
    END LOOP;
END $$;

ALTER TABLE strategy_orders DROP CONSTRAINT IF EXISTS strategy_orders_role_check;
ALTER TABLE strategy_orders ADD CONSTRAINT strategy_orders_role_check CHECK (
    role IN ('BUY_ENTRY','SELL_ENTRY','TARGET','SL1','SL2','EMERGENCY_CLOSE')
);

-- A partially filled entry may need multiple independently acknowledged stop
-- slices.  A one-row-per-stop unique index would leave later/late fill deltas
-- unprotected.  Preflight instead rejects exit coverage greater than the
-- actual local exposure and the trigger below serializes future checks on the
-- trade row.
DROP INDEX IF EXISTS strategy_orders_active_trade_exit_role_idx;

DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM strategy_orders o
        JOIN trades t ON t.id=o.trade_id
        WHERE o.role IN ('TARGET','SL1','SL2')
          AND o.status IN ('pending','submitting','ambiguous','submitted','partially_filled','processing','cancelling')
        GROUP BY o.trade_id,
                 CASE WHEN o.role IN ('SL1','SL2') THEN 'STOP' ELSE 'TARGET' END
        HAVING SUM(GREATEST(o.quantity-o.processed_quantity,0)) > MAX(t.quantity)
    ) OR EXISTS (
        SELECT 1
        FROM strategy_orders o
        WHERE o.role IN ('TARGET','SL1','SL2','EMERGENCY_CLOSE')
          AND o.status IN ('pending','submitting','ambiguous','submitted','partially_filled','processing','cancelling')
        GROUP BY o.trade_id
        HAVING COUNT(*) FILTER (WHERE o.role='EMERGENCY_CLOSE') > 1
            OR (
                COUNT(*) FILTER (WHERE o.role='EMERGENCY_CLOSE') > 0
                AND COUNT(*) FILTER (WHERE o.role<>'EMERGENCY_CLOSE') > 0
            )
    ) THEN
        RAISE EXCEPTION 'unsafe active exit coverage must be broker-reconciled before execution safety migration';
    END IF;
END $$;

CREATE OR REPLACE FUNCTION enforce_strategy_exit_coverage()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
DECLARE
    trade_quantity INTEGER;
    active_quantity BIGINT;
BEGIN
    IF NEW.trade_id IS NULL
       OR NEW.role NOT IN ('TARGET','SL1','SL2','EMERGENCY_CLOSE')
       OR NEW.status NOT IN ('pending','submitting','ambiguous','submitted','partially_filled','processing','cancelling') THEN
        RETURN NEW;
    END IF;

    SELECT quantity INTO trade_quantity
    FROM trades
    WHERE id=NEW.trade_id AND status='open'
    FOR UPDATE;
    IF trade_quantity IS NULL THEN
        RAISE EXCEPTION 'active exit % has no open trade exposure', NEW.id;
    END IF;

    IF NEW.role='EMERGENCY_CLOSE' THEN
        IF EXISTS (
            SELECT 1 FROM strategy_orders o
            WHERE o.trade_id=NEW.trade_id AND o.id<>NEW.id
              AND o.role IN ('TARGET','SL1','SL2','EMERGENCY_CLOSE')
              AND o.status IN ('pending','submitting','ambiguous','submitted','partially_filled','processing','cancelling')
        ) THEN
            RAISE EXCEPTION 'emergency close cannot overlap another active exit for trade %', NEW.trade_id;
        END IF;
    ELSIF EXISTS (
        SELECT 1 FROM strategy_orders o
        WHERE o.trade_id=NEW.trade_id AND o.id<>NEW.id
          AND o.role='EMERGENCY_CLOSE'
          AND o.status IN ('pending','submitting','ambiguous','submitted','partially_filled','processing','cancelling')
    ) THEN
        RAISE EXCEPTION 'normal exit cannot overlap an active emergency close for trade %', NEW.trade_id;
    END IF;

    SELECT COALESCE(SUM(GREATEST(o.quantity-o.processed_quantity,0)),0)
    INTO active_quantity
    FROM strategy_orders o
    WHERE o.trade_id=NEW.trade_id AND o.id<>NEW.id
      AND o.status IN ('pending','submitting','ambiguous','submitted','partially_filled','processing','cancelling')
      AND CASE
            WHEN NEW.role IN ('SL1','SL2') THEN o.role IN ('SL1','SL2')
            ELSE o.role=NEW.role
          END;
    active_quantity := active_quantity + GREATEST(NEW.quantity-NEW.processed_quantity,0);
    IF active_quantity > trade_quantity THEN
        RAISE EXCEPTION 'active % coverage % exceeds trade % quantity %',
            NEW.role, active_quantity, NEW.trade_id, trade_quantity;
    END IF;
    RETURN NEW;
END $$;

DROP TRIGGER IF EXISTS strategy_orders_exit_coverage_guard ON strategy_orders;
CREATE TRIGGER strategy_orders_exit_coverage_guard
BEFORE INSERT OR UPDATE OF trade_id,role,status,quantity,processed_quantity
ON strategy_orders
FOR EACH ROW
EXECUTE FUNCTION enforce_strategy_exit_coverage();

CREATE TABLE IF NOT EXISTS broker_position_incidents (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    strategy_key VARCHAR(64) NOT NULL,
    instrument VARCHAR(64) NOT NULL,
    exchange_segment VARCHAR(16) NOT NULL,
    contract_token VARCHAR(32) NOT NULL,
    contract_symbol VARCHAR(96) NOT NULL DEFAULT '',
    incident_type VARCHAR(40) NOT NULL,
    status VARCHAR(24) NOT NULL DEFAULT 'open' CHECK (status IN ('open','resolved','operator_required')),
    broker_quantity INTEGER NOT NULL DEFAULT 0,
    local_quantity INTEGER NOT NULL DEFAULT 0,
    broker_average_price DOUBLE PRECISION,
    trade_id UUID REFERENCES trades(id) ON DELETE SET NULL,
    detail TEXT NOT NULL DEFAULT '',
    first_detected_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_detected_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    resolved_at TIMESTAMPTZ,
    UNIQUE (user_id, exchange_segment, contract_token, incident_type)
);

CREATE INDEX IF NOT EXISTS broker_position_incidents_open_idx
    ON broker_position_incidents (user_id, status, last_detected_at DESC)
    WHERE status IN ('open','operator_required');

-- <<< END: backend/migrations/20260823010000_execution_safety_lifecycle.sql >>>


-- >>> START: backend/migrations/20260823020000_p0_execution_safety.sql <<<
-- Columns used by the executable order model must exist on both clean and
-- upgraded databases.  Historical margin migrations added order_type only to
-- the removed margin cache, leaving strategy_orders without the field.
ALTER TABLE strategy_orders
    ADD COLUMN IF NOT EXISTS order_type VARCHAR(32) NOT NULL DEFAULT 'STOPLOSS_LIMIT',
    ADD COLUMN IF NOT EXISTS exchange_segment VARCHAR(16) NOT NULL DEFAULT 'MCX',
    ADD COLUMN IF NOT EXISTS product_type VARCHAR(32) NOT NULL DEFAULT 'CARRYFORWARD';

ALTER TABLE broker_position_incidents
    ADD COLUMN IF NOT EXISTS product_type VARCHAR(32) NOT NULL DEFAULT '',
    ADD COLUMN IF NOT EXISTS ownership_status VARCHAR(32) NOT NULL DEFAULT 'strategy_related',
    ADD COLUMN IF NOT EXISTS raw_broker_position JSONB NOT NULL DEFAULT '{}';

ALTER TABLE broker_position_incidents DROP CONSTRAINT IF EXISTS broker_position_incidents_ownership_status_check;
ALTER TABLE broker_position_incidents ADD CONSTRAINT broker_position_incidents_ownership_status_check
    CHECK (ownership_status IN ('strategy_related','detected_unattributed'));

ALTER TABLE trades
    ADD COLUMN IF NOT EXISTS exposure_origin VARCHAR(32) NOT NULL DEFAULT 'strategy_entry';

ALTER TABLE trades DROP CONSTRAINT IF EXISTS trades_exposure_origin_check;
ALTER TABLE trades ADD CONSTRAINT trades_exposure_origin_check
    CHECK (exposure_origin IN ('strategy_entry','broker_over_close'));

CREATE OR REPLACE FUNCTION enforce_strategy_order_status_transition()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    IF NEW.status=OLD.status THEN
        RETURN NEW;
    END IF;
    IF NOT (
        (OLD.status='pending' AND NEW.status IN ('submitting','submitted','failed','rejected','cancelled'))
        OR (OLD.status='submitting' AND NEW.status IN ('submitted','ambiguous','failed','rejected','cancelled'))
        OR (OLD.status='ambiguous' AND NEW.status IN ('submitted','partially_filled','processing','filled','rejected','cancelled','cancelling'))
        OR (OLD.status='submitted' AND NEW.status IN ('partially_filled','processing','filled','rejected','cancelled','cancelling'))
        OR (OLD.status='partially_filled' AND NEW.status IN ('submitted','processing','filled','rejected','cancelled','cancelling'))
        OR (OLD.status='processing' AND NEW.status IN ('submitted','partially_filled','filled','rejected','cancelled','cancelling'))
        OR (OLD.status='cancelling' AND NEW.status IN ('submitted','partially_filled','processing','filled','rejected','cancelled'))
        OR (OLD.status='failed' AND NEW.status='pending')
    ) THEN
        RAISE EXCEPTION 'invalid strategy order transition % -> % for %', OLD.status, NEW.status, OLD.id;
    END IF;
    RETURN NEW;
END $$;

DROP TRIGGER IF EXISTS strategy_orders_status_transition_guard ON strategy_orders;
CREATE TRIGGER strategy_orders_status_transition_guard
BEFORE UPDATE OF status ON strategy_orders
FOR EACH ROW
EXECUTE FUNCTION enforce_strategy_order_status_transition();

CREATE OR REPLACE FUNCTION enforce_trade_safety_terminal_transition()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    IF OLD.safety_status='CLOSED' AND NEW.safety_status<>'CLOSED' THEN
        RAISE EXCEPTION 'closed trade safety state cannot regress for %', OLD.id;
    END IF;
    IF OLD.safety_status='EMERGENCY_CLOSING'
       AND NEW.safety_status IN ('DEMO','PROTECTION_REQUIRED','PROTECTION_SUBMITTING','PROTECTION_UNCERTAIN','PROTECTED','PROTECTION_FAILED','CLOSING') THEN
        RAISE EXCEPTION 'emergency-closing trade safety state cannot regress to % for %', NEW.safety_status, OLD.id;
    END IF;
    RETURN NEW;
END $$;

DROP TRIGGER IF EXISTS trades_safety_terminal_transition_guard ON trades;
CREATE TRIGGER trades_safety_terminal_transition_guard
BEFORE UPDATE OF safety_status ON trades
FOR EACH ROW
EXECUTE FUNCTION enforce_trade_safety_terminal_transition();

-- <<< END: backend/migrations/20260823020000_p0_execution_safety.sql >>>


-- >>> START: backend/migrations/20260824000000_admin_clear_demo_reset_fence.sql <<<
-- An Admin Clear Trades reset is a durable boundary.  Entry signals that
-- originated before this timestamp must never recreate disposable demo state
-- after the maintenance transaction commits.
ALTER TABLE user_profiles
    ADD COLUMN IF NOT EXISTS demo_state_reset_at TIMESTAMPTZ;

-- <<< END: backend/migrations/20260824000000_admin_clear_demo_reset_fence.sql >>>


-- >>> START: backend/migrations/20260825000000_widen_futures_gap_directions.sql <<<
ALTER TABLE strategy_market_snapshots
    ALTER COLUMN gap_direction TYPE VARCHAR(16),
    ALTER COLUMN entry_direction TYPE VARCHAR(16);

-- <<< END: backend/migrations/20260825000000_widen_futures_gap_directions.sql >>>


-- >>> START: backend/migrations/20260826000000_angel_static_egress_ips.sql <<<
CREATE TABLE broker_egress_ips (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    ip_address INET NOT NULL,
    configuration_status VARCHAR(32) NOT NULL DEFAULT 'NOT_CONFIGURED'
        CHECK (configuration_status IN ('NOT_CONFIGURED','CONFIGURING','CONFIGURED','CONFIGURATION_FAILED')),
    verification_status VARCHAR(32) NOT NULL DEFAULT 'UNVERIFIED'
        CHECK (verification_status IN ('UNVERIFIED','VERIFYING','VERIFIED','VERIFICATION_FAILED')),
    status_message VARCHAR(512) NOT NULL DEFAULT '',
    configured_at TIMESTAMPTZ,
    last_verified_at TIMESTAMPTZ,
    created_by UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT broker_egress_ips_ipv4_only CHECK (family(ip_address) = 4),
    CONSTRAINT broker_egress_ips_host_address CHECK (masklen(ip_address) = 32)
);

CREATE UNIQUE INDEX broker_egress_ips_address_unique
    ON broker_egress_ips (ip_address);

ALTER TABLE user_profiles
    ADD COLUMN broker_egress_ip_id UUID
    REFERENCES broker_egress_ips(id) ON DELETE RESTRICT;

-- A purchased dedicated source address is exclusive to one Angel account.
-- This is the final race-safety boundary for concurrent admin assignments.
CREATE UNIQUE INDEX user_profiles_broker_egress_ip_unique
    ON user_profiles (broker_egress_ip_id)
    WHERE broker_egress_ip_id IS NOT NULL;

CREATE INDEX broker_egress_ips_status_idx
    ON broker_egress_ips (configuration_status, verification_status);

COMMENT ON TABLE broker_egress_ips IS
    'Approved public IPv4 inventory for explicit Angel One source-address binding.';
COMMENT ON COLUMN user_profiles.broker_egress_ip_id IS
    'NULL uses normal OS networking; non-NULL requires verified fail-closed Angel source binding.';

-- <<< END: backend/migrations/20260826000000_angel_static_egress_ips.sql >>>


-- >>> START: backend/migrations/20260904000000_manual_trade_close.sql <<<
CREATE TABLE IF NOT EXISTS manual_trade_close_intents (
    trade_id UUID PRIMARY KEY REFERENCES trades(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    status VARCHAR(32) NOT NULL DEFAULT 'requested'
        CHECK (status IN (
            'requested',
            'cancelling_protection',
            'submitted',
            'partially_filled',
            'ambiguous',
            'completed',
            'failed',
            'reconciliation_required'
        )),
    requested_quantity INTEGER NOT NULL CHECK (requested_quantity > 0),
    close_side VARCHAR(4) NOT NULL CHECK (close_side IN ('BUY', 'SELL')),
    strategy_order_id UUID REFERENCES strategy_orders(id) ON DELETE SET NULL,
    last_error TEXT NOT NULL DEFAULT '',
    requested_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    completed_at TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS manual_trade_close_intents_status_idx
    ON manual_trade_close_intents (status, updated_at)
    WHERE status <> 'completed';

-- <<< END: backend/migrations/20260904000000_manual_trade_close.sql >>>


-- >>> START: backend/migrations/20260904010000_broker_deployment_readiness.sql <<<
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

-- <<< END: backend/migrations/20260904010000_broker_deployment_readiness.sql >>>


-- >>> START: backend/migrations/20260912000000_durable_manual_broker_close_evidence.sql <<<
ALTER TABLE trades
    ADD COLUMN IF NOT EXISTS last_exact_broker_exposure_at TIMESTAMPTZ;

UPDATE trades AS trade
SET last_exact_broker_exposure_at = trade.last_position_reconciled_at
FROM strategy_market_snapshots AS snapshot
WHERE snapshot.id = trade.strategy_snapshot_id
  AND trade.execution_mode = 'live'
  AND trade.status = 'open'
  AND trade.last_exact_broker_exposure_at IS NULL
  AND trade.last_position_reconciled_at IS NOT NULL
  AND trade.broker_net_quantity = CASE WHEN trade.direction = 'BUY' THEN trade.quantity ELSE -trade.quantity END
  AND NOT EXISTS (
      SELECT 1
      FROM trades AS other_trade
      JOIN strategy_market_snapshots AS other_snapshot
        ON other_snapshot.id = other_trade.strategy_snapshot_id
      WHERE other_trade.id <> trade.id
        AND other_trade.user_id = trade.user_id
        AND other_trade.execution_mode = 'live'
        AND other_trade.status = 'open'
        AND UPPER(other_snapshot.exchange_segment) = UPPER(snapshot.exchange_segment)
        AND other_snapshot.contract_token = snapshot.contract_token
  );

CREATE TABLE IF NOT EXISTS manual_broker_close_evidence (
    trade_id UUID PRIMARY KEY REFERENCES trades(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    broker_credential_revision BIGINT NOT NULL,
    exchange_segment VARCHAR(16) NOT NULL,
    contract_token VARCHAR(32) NOT NULL,
    contract_symbol VARCHAR(96) NOT NULL DEFAULT '',
    close_side VARCHAR(4) NOT NULL CHECK (close_side IN ('BUY', 'SELL')),
    filled_quantity INTEGER NOT NULL CHECK (filled_quantity > 0),
    weighted_fill_price NUMERIC(20, 6) NOT NULL CHECK (weighted_fill_price > 0),
    broker_order_ids TEXT[] NOT NULL CHECK (cardinality(broker_order_ids) > 0),
    first_fill_at TIMESTAMPTZ NOT NULL,
    last_fill_at TIMESTAMPTZ NOT NULL,
    observed_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    consumed_at TIMESTAMPTZ,
    CHECK (last_fill_at >= first_fill_at)
);

CREATE INDEX IF NOT EXISTS manual_broker_close_evidence_pending_idx
    ON manual_broker_close_evidence (user_id, observed_at)
    WHERE consumed_at IS NULL;

COMMENT ON COLUMN trades.last_exact_broker_exposure_at IS
    'Latest authoritative reconciliation at which broker net exposure exactly matched this sole open LIVE trade.';

COMMENT ON TABLE manual_broker_close_evidence IS
    'Durable exact external-fill evidence scoped to one broker credential revision and retained while protective-order cleanup may require later reconciliation passes.';

-- <<< END: backend/migrations/20260912000000_durable_manual_broker_close_evidence.sql >>>


-- >>> START: backend/migrations/20260918000000_ownership_aware_broker_exposure.sql <<<
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

-- <<< END: backend/migrations/20260918000000_ownership_aware_broker_exposure.sql >>>


-- >>> START: backend/migrations/20260921000000_live_mutation_authority.sql <<<
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

-- <<< END: backend/migrations/20260921000000_live_mutation_authority.sql >>>

