CREATE TABLE broker_egress_ips (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    ip_address INET NOT NULL,
    configuration_status VARCHAR(32) NOT NULL DEFAULT 'NOT_CONFIGURED'
        CHECK (configuration_status IN ('NOT_CONFIGURED','CONFIGURING','CONFIGURED','CONFIGURATION_FAILED')),
    verification_status VARCHAR(32) NOT NULL DEFAULT 'UNVERIFIED'
        CHECK (verification_status IN ('UNVERIFIED','VERIFYING','VERIFIED','VERIFICATION_FAILED')),
    status_message VARCHAR(512) NOT NULL DEFAULT '',
    configured_at TIMESTAMPTZ,
    last_verified_at TIMESTAMPTZ,
    created_by UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT broker_egress_ips_ipv4_only CHECK (family(ip_address) = 4),
    CONSTRAINT broker_egress_ips_host_address CHECK (masklen(ip_address) = 32)
);

CREATE UNIQUE INDEX broker_egress_ips_address_unique
    ON broker_egress_ips (ip_address);

ALTER TABLE user_profiles
    ADD COLUMN broker_egress_ip_id UUID
    REFERENCES broker_egress_ips(id) ON DELETE RESTRICT;

-- A purchased dedicated source address is exclusive to one Angel account.
-- This is the final race-safety boundary for concurrent admin assignments.
CREATE UNIQUE INDEX user_profiles_broker_egress_ip_unique
    ON user_profiles (broker_egress_ip_id)
    WHERE broker_egress_ip_id IS NOT NULL;

CREATE INDEX broker_egress_ips_status_idx
    ON broker_egress_ips (configuration_status, verification_status);

COMMENT ON TABLE broker_egress_ips IS
    'Approved public IPv4 inventory for explicit Angel One source-address binding.';
COMMENT ON COLUMN user_profiles.broker_egress_ip_id IS
    'NULL uses normal OS networking; non-NULL requires verified fail-closed Angel source binding.';
