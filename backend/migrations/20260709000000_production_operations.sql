CREATE TABLE IF NOT EXISTS audit_events (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    event_type VARCHAR(96) NOT NULL,
    actor_user_id UUID REFERENCES users(id) ON DELETE SET NULL,
    target_user_id UUID REFERENCES users(id) ON DELETE SET NULL,
    request_id VARCHAR(128),
    correlation_id VARCHAR(128),
    ip_address INET,
    summary TEXT NOT NULL DEFAULT '',
    metadata JSONB NOT NULL DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS audit_events_type_created_idx ON audit_events (event_type, created_at DESC);
CREATE INDEX IF NOT EXISTS audit_events_actor_created_idx ON audit_events (actor_user_id, created_at DESC);
CREATE INDEX IF NOT EXISTS audit_events_target_created_idx ON audit_events (target_user_id, created_at DESC);

CREATE OR REPLACE FUNCTION reject_audit_event_mutation()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    RAISE EXCEPTION 'audit_events is append-only';
END;
$$;

DROP TRIGGER IF EXISTS audit_events_no_update ON audit_events;
CREATE TRIGGER audit_events_no_update
BEFORE UPDATE ON audit_events
FOR EACH ROW EXECUTE FUNCTION reject_audit_event_mutation();

DROP TRIGGER IF EXISTS audit_events_no_delete ON audit_events;
CREATE TRIGGER audit_events_no_delete
BEFORE DELETE ON audit_events
FOR EACH ROW EXECUTE FUNCTION reject_audit_event_mutation();

CREATE TABLE IF NOT EXISTS alert_delivery_attempts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    event_type VARCHAR(96) NOT NULL,
    severity VARCHAR(16) NOT NULL,
    channel VARCHAR(32) NOT NULL,
    destination TEXT NOT NULL DEFAULT '',
    status VARCHAR(16) NOT NULL CHECK (status IN ('sent','failed','skipped')),
    error TEXT NOT NULL DEFAULT '',
    payload JSONB NOT NULL DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS alert_delivery_attempts_created_idx
    ON alert_delivery_attempts (created_at DESC);
