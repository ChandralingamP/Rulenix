ALTER TABLE trades
    ADD COLUMN IF NOT EXISTS safety_status VARCHAR(32) NOT NULL DEFAULT 'DEMO',
    ADD COLUMN IF NOT EXISTS protection_deadline_at TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS protection_attempts INTEGER NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS last_protection_error TEXT NOT NULL DEFAULT '',
    ADD COLUMN IF NOT EXISTS broker_net_quantity INTEGER,
    ADD COLUMN IF NOT EXISTS broker_average_price DOUBLE PRECISION,
    ADD COLUMN IF NOT EXISTS last_position_reconciled_at TIMESTAMPTZ;

ALTER TABLE strategy_orders
    ADD COLUMN IF NOT EXISTS uncertain_since_at TIMESTAMPTZ;

UPDATE trades
SET safety_status = CASE
    WHEN status = 'closed' THEN 'CLOSED'
    WHEN execution_mode = 'demo' THEN 'DEMO'
    ELSE 'PROTECTION_REQUIRED'
END
WHERE safety_status = 'DEMO';

ALTER TABLE trades DROP CONSTRAINT IF EXISTS trades_safety_status_check;
ALTER TABLE trades ADD CONSTRAINT trades_safety_status_check CHECK (safety_status IN (
    'DEMO',
    'PROTECTION_REQUIRED',
    'PROTECTION_SUBMITTING',
    'PROTECTION_UNCERTAIN',
    'PROTECTED',
    'PROTECTION_FAILED',
    'CLOSING',
    'EMERGENCY_CLOSING',
    'RECONCILIATION_REQUIRED',
    'CLOSED'
));

ALTER TABLE trades DROP CONSTRAINT IF EXISTS trades_protection_attempts_check;
ALTER TABLE trades ADD CONSTRAINT trades_protection_attempts_check
    CHECK (protection_attempts >= 0);

CREATE INDEX IF NOT EXISTS trades_execution_safety_idx
    ON trades (execution_mode, safety_status, updated_at)
    WHERE status = 'open';

DO $$
DECLARE
    constraint_name TEXT;
BEGIN
    FOR constraint_name IN
        SELECT conname
        FROM pg_constraint
        WHERE conrelid = 'strategy_orders'::regclass
          AND contype = 'c'
          AND pg_get_constraintdef(oid) LIKE '%role%BUY_ENTRY%'
    LOOP
        EXECUTE format('ALTER TABLE strategy_orders DROP CONSTRAINT %I', constraint_name);
    END LOOP;
END $$;

ALTER TABLE strategy_orders DROP CONSTRAINT IF EXISTS strategy_orders_role_check;
ALTER TABLE strategy_orders ADD CONSTRAINT strategy_orders_role_check CHECK (
    role IN ('BUY_ENTRY','SELL_ENTRY','TARGET','SL1','SL2','EMERGENCY_CLOSE')
);

-- A partially filled entry may need multiple independently acknowledged stop
-- slices.  A one-row-per-stop unique index would leave later/late fill deltas
-- unprotected.  Preflight instead rejects exit coverage greater than the
-- actual local exposure and the trigger below serializes future checks on the
-- trade row.
DROP INDEX IF EXISTS strategy_orders_active_trade_exit_role_idx;

DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM strategy_orders o
        JOIN trades t ON t.id=o.trade_id
        WHERE o.role IN ('TARGET','SL1','SL2')
          AND o.status IN ('pending','submitting','ambiguous','submitted','partially_filled','processing','cancelling')
        GROUP BY o.trade_id,
                 CASE WHEN o.role IN ('SL1','SL2') THEN 'STOP' ELSE 'TARGET' END
        HAVING SUM(GREATEST(o.quantity-o.processed_quantity,0)) > MAX(t.quantity)
    ) OR EXISTS (
        SELECT 1
        FROM strategy_orders o
        WHERE o.role IN ('TARGET','SL1','SL2','EMERGENCY_CLOSE')
          AND o.status IN ('pending','submitting','ambiguous','submitted','partially_filled','processing','cancelling')
        GROUP BY o.trade_id
        HAVING COUNT(*) FILTER (WHERE o.role='EMERGENCY_CLOSE') > 1
            OR (
                COUNT(*) FILTER (WHERE o.role='EMERGENCY_CLOSE') > 0
                AND COUNT(*) FILTER (WHERE o.role<>'EMERGENCY_CLOSE') > 0
            )
    ) THEN
        RAISE EXCEPTION 'unsafe active exit coverage must be broker-reconciled before execution safety migration';
    END IF;
END $$;

CREATE OR REPLACE FUNCTION enforce_strategy_exit_coverage()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
DECLARE
    trade_quantity INTEGER;
    active_quantity BIGINT;
BEGIN
    IF NEW.trade_id IS NULL
       OR NEW.role NOT IN ('TARGET','SL1','SL2','EMERGENCY_CLOSE')
       OR NEW.status NOT IN ('pending','submitting','ambiguous','submitted','partially_filled','processing','cancelling') THEN
        RETURN NEW;
    END IF;

    SELECT quantity INTO trade_quantity
    FROM trades
    WHERE id=NEW.trade_id AND status='open'
    FOR UPDATE;
    IF trade_quantity IS NULL THEN
        RAISE EXCEPTION 'active exit % has no open trade exposure', NEW.id;
    END IF;

    IF NEW.role='EMERGENCY_CLOSE' THEN
        IF EXISTS (
            SELECT 1 FROM strategy_orders o
            WHERE o.trade_id=NEW.trade_id AND o.id<>NEW.id
              AND o.role IN ('TARGET','SL1','SL2','EMERGENCY_CLOSE')
              AND o.status IN ('pending','submitting','ambiguous','submitted','partially_filled','processing','cancelling')
        ) THEN
            RAISE EXCEPTION 'emergency close cannot overlap another active exit for trade %', NEW.trade_id;
        END IF;
    ELSIF EXISTS (
        SELECT 1 FROM strategy_orders o
        WHERE o.trade_id=NEW.trade_id AND o.id<>NEW.id
          AND o.role='EMERGENCY_CLOSE'
          AND o.status IN ('pending','submitting','ambiguous','submitted','partially_filled','processing','cancelling')
    ) THEN
        RAISE EXCEPTION 'normal exit cannot overlap an active emergency close for trade %', NEW.trade_id;
    END IF;

    SELECT COALESCE(SUM(GREATEST(o.quantity-o.processed_quantity,0)),0)
    INTO active_quantity
    FROM strategy_orders o
    WHERE o.trade_id=NEW.trade_id AND o.id<>NEW.id
      AND o.status IN ('pending','submitting','ambiguous','submitted','partially_filled','processing','cancelling')
      AND CASE
            WHEN NEW.role IN ('SL1','SL2') THEN o.role IN ('SL1','SL2')
            ELSE o.role=NEW.role
          END;
    active_quantity := active_quantity + GREATEST(NEW.quantity-NEW.processed_quantity,0);
    IF active_quantity > trade_quantity THEN
        RAISE EXCEPTION 'active % coverage % exceeds trade % quantity %',
            NEW.role, active_quantity, NEW.trade_id, trade_quantity;
    END IF;
    RETURN NEW;
END $$;

DROP TRIGGER IF EXISTS strategy_orders_exit_coverage_guard ON strategy_orders;
CREATE TRIGGER strategy_orders_exit_coverage_guard
BEFORE INSERT OR UPDATE OF trade_id,role,status,quantity,processed_quantity
ON strategy_orders
FOR EACH ROW
EXECUTE FUNCTION enforce_strategy_exit_coverage();

CREATE TABLE IF NOT EXISTS broker_position_incidents (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    strategy_key VARCHAR(64) NOT NULL,
    instrument VARCHAR(64) NOT NULL,
    exchange_segment VARCHAR(16) NOT NULL,
    contract_token VARCHAR(32) NOT NULL,
    contract_symbol VARCHAR(96) NOT NULL DEFAULT '',
    incident_type VARCHAR(40) NOT NULL,
    status VARCHAR(24) NOT NULL DEFAULT 'open' CHECK (status IN ('open','resolved','operator_required')),
    broker_quantity INTEGER NOT NULL DEFAULT 0,
    local_quantity INTEGER NOT NULL DEFAULT 0,
    broker_average_price DOUBLE PRECISION,
    trade_id UUID REFERENCES trades(id) ON DELETE SET NULL,
    detail TEXT NOT NULL DEFAULT '',
    first_detected_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_detected_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    resolved_at TIMESTAMPTZ,
    UNIQUE (user_id, exchange_segment, contract_token, incident_type)
);

CREATE INDEX IF NOT EXISTS broker_position_incidents_open_idx
    ON broker_position_incidents (user_id, status, last_detected_at DESC)
    WHERE status IN ('open','operator_required');
