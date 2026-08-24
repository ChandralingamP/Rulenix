-- An Admin Clear Trades reset is a durable boundary.  Entry signals that
-- originated before this timestamp must never recreate disposable demo state
-- after the maintenance transaction commits.
ALTER TABLE user_profiles
    ADD COLUMN IF NOT EXISTS demo_state_reset_at TIMESTAMPTZ;

