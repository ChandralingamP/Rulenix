ALTER TABLE user_profiles
    ADD COLUMN IF NOT EXISTS broker_credential_revision BIGINT NOT NULL DEFAULT 0;
