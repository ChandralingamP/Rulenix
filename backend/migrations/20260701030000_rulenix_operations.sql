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
