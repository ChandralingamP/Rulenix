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
