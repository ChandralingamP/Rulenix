ALTER TABLE users ADD COLUMN IF NOT EXISTS failed_login_attempts INTEGER NOT NULL DEFAULT 0;
ALTER TABLE users ADD COLUMN IF NOT EXISTS locked_until TIMESTAMPTZ;
ALTER TABLE users ADD COLUMN IF NOT EXISTS last_failed_login_at TIMESTAMPTZ;

ALTER TABLE email_otps ADD COLUMN IF NOT EXISTS attempt_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE email_otps ADD COLUMN IF NOT EXISTS invalidated_at TIMESTAMPTZ;
CREATE INDEX IF NOT EXISTS email_otps_active_lookup_idx
    ON email_otps (LOWER(email), purpose, created_at DESC)
    WHERE is_used = FALSE AND invalidated_at IS NULL;
