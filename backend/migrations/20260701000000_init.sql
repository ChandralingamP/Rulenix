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
