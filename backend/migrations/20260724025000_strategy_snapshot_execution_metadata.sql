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
