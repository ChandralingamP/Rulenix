ALTER TABLE user_profiles
    ADD COLUMN IF NOT EXISTS demo_balance NUMERIC(18,2) NOT NULL DEFAULT 200000.00;

UPDATE user_profiles SET demo_balance = 200000.00 WHERE demo_balance IS NULL;

ALTER TABLE email_otps DROP CONSTRAINT IF EXISTS email_otps_purpose_check;
ALTER TABLE email_otps
    ADD CONSTRAINT email_otps_purpose_check
    CHECK (purpose IN ('signup', 'password_reset', 'profile_update'));
