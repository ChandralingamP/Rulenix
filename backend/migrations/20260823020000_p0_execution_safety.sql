-- Columns used by the executable order model must exist on both clean and
-- upgraded databases.  Historical margin migrations added order_type only to
-- the removed margin cache, leaving strategy_orders without the field.
ALTER TABLE strategy_orders
    ADD COLUMN IF NOT EXISTS order_type VARCHAR(32) NOT NULL DEFAULT 'STOPLOSS_LIMIT',
    ADD COLUMN IF NOT EXISTS exchange_segment VARCHAR(16) NOT NULL DEFAULT 'MCX',
    ADD COLUMN IF NOT EXISTS product_type VARCHAR(32) NOT NULL DEFAULT 'CARRYFORWARD';

ALTER TABLE broker_position_incidents
    ADD COLUMN IF NOT EXISTS product_type VARCHAR(32) NOT NULL DEFAULT '',
    ADD COLUMN IF NOT EXISTS ownership_status VARCHAR(32) NOT NULL DEFAULT 'strategy_related',
    ADD COLUMN IF NOT EXISTS raw_broker_position JSONB NOT NULL DEFAULT '{}';

ALTER TABLE broker_position_incidents DROP CONSTRAINT IF EXISTS broker_position_incidents_ownership_status_check;
ALTER TABLE broker_position_incidents ADD CONSTRAINT broker_position_incidents_ownership_status_check
    CHECK (ownership_status IN ('strategy_related','detected_unattributed'));

ALTER TABLE trades
    ADD COLUMN IF NOT EXISTS exposure_origin VARCHAR(32) NOT NULL DEFAULT 'strategy_entry';

ALTER TABLE trades DROP CONSTRAINT IF EXISTS trades_exposure_origin_check;
ALTER TABLE trades ADD CONSTRAINT trades_exposure_origin_check
    CHECK (exposure_origin IN ('strategy_entry','broker_over_close'));

CREATE OR REPLACE FUNCTION enforce_strategy_order_status_transition()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    IF NEW.status=OLD.status THEN
        RETURN NEW;
    END IF;
    IF NOT (
        (OLD.status='pending' AND NEW.status IN ('submitting','submitted','failed','rejected','cancelled'))
        OR (OLD.status='submitting' AND NEW.status IN ('submitted','ambiguous','failed','rejected','cancelled'))
        OR (OLD.status='ambiguous' AND NEW.status IN ('submitted','partially_filled','processing','filled','rejected','cancelled','cancelling'))
        OR (OLD.status='submitted' AND NEW.status IN ('partially_filled','processing','filled','rejected','cancelled','cancelling'))
        OR (OLD.status='partially_filled' AND NEW.status IN ('submitted','processing','filled','rejected','cancelled','cancelling'))
        OR (OLD.status='processing' AND NEW.status IN ('submitted','partially_filled','filled','rejected','cancelled','cancelling'))
        OR (OLD.status='cancelling' AND NEW.status IN ('submitted','partially_filled','processing','filled','rejected','cancelled'))
        OR (OLD.status='failed' AND NEW.status='pending')
    ) THEN
        RAISE EXCEPTION 'invalid strategy order transition % -> % for %', OLD.status, NEW.status, OLD.id;
    END IF;
    RETURN NEW;
END $$;

DROP TRIGGER IF EXISTS strategy_orders_status_transition_guard ON strategy_orders;
CREATE TRIGGER strategy_orders_status_transition_guard
BEFORE UPDATE OF status ON strategy_orders
FOR EACH ROW
EXECUTE FUNCTION enforce_strategy_order_status_transition();

CREATE OR REPLACE FUNCTION enforce_trade_safety_terminal_transition()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    IF OLD.safety_status='CLOSED' AND NEW.safety_status<>'CLOSED' THEN
        RAISE EXCEPTION 'closed trade safety state cannot regress for %', OLD.id;
    END IF;
    IF OLD.safety_status='EMERGENCY_CLOSING'
       AND NEW.safety_status IN ('DEMO','PROTECTION_REQUIRED','PROTECTION_SUBMITTING','PROTECTION_UNCERTAIN','PROTECTED','PROTECTION_FAILED','CLOSING') THEN
        RAISE EXCEPTION 'emergency-closing trade safety state cannot regress to % for %', NEW.safety_status, OLD.id;
    END IF;
    RETURN NEW;
END $$;

DROP TRIGGER IF EXISTS trades_safety_terminal_transition_guard ON trades;
CREATE TRIGGER trades_safety_terminal_transition_guard
BEFORE UPDATE OF safety_status ON trades
FOR EACH ROW
EXECUTE FUNCTION enforce_trade_safety_terminal_transition();
