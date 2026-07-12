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
