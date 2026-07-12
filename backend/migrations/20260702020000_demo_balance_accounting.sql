ALTER TABLE user_profiles
    ALTER COLUMN demo_balance SET DEFAULT 200000.00;

-- Keep existing demo accounts at the configured starting balance unless users
-- have already topped up or changed them.
UPDATE user_profiles
SET demo_balance = 200000.00,
    updated_at = NOW()
WHERE demo_balance IS NULL;
