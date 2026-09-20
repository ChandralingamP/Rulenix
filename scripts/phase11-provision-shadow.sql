\set ON_ERROR_STOP on

\if :{?shadow_reader_password}
\else
  \echo 'shadow_reader_password is required'
  \quit 2
\endif
\if :{?shadow_writer_password}
\else
  \echo 'shadow_writer_password is required'
  \quit 2
\endif

DO $$
BEGIN
  IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname='rulenix_shadow_owner') THEN
    CREATE ROLE rulenix_shadow_owner NOLOGIN NOINHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;
  END IF;
  IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname='rulenix_shadow_reader') THEN
    CREATE ROLE rulenix_shadow_reader LOGIN NOINHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;
  END IF;
  IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname='rulenix_shadow_writer') THEN
    CREATE ROLE rulenix_shadow_writer LOGIN NOINHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;
  END IF;
END
$$;

ALTER ROLE rulenix_shadow_reader PASSWORD :'shadow_reader_password';
ALTER ROLE rulenix_shadow_writer PASSWORD :'shadow_writer_password';
ALTER ROLE rulenix_shadow_reader CONNECTION LIMIT 3;
ALTER ROLE rulenix_shadow_writer CONNECTION LIMIT 3;
ALTER ROLE rulenix_shadow_reader SET default_transaction_read_only=on;
ALTER ROLE rulenix_shadow_reader SET statement_timeout='5s';
ALTER ROLE rulenix_shadow_reader SET lock_timeout='1s';
ALTER ROLE rulenix_shadow_writer SET statement_timeout='5s';
ALTER ROLE rulenix_shadow_writer SET lock_timeout='1s';

SELECT format(
  'GRANT CONNECT ON DATABASE %I TO rulenix_shadow_reader,rulenix_shadow_writer',
  current_database()
) \gexec
CREATE SCHEMA IF NOT EXISTS rulenix_shadow AUTHORIZATION rulenix_shadow_owner;
ALTER SCHEMA rulenix_shadow OWNER TO rulenix_shadow_owner;
REVOKE ALL ON SCHEMA rulenix_shadow FROM PUBLIC;
GRANT USAGE ON SCHEMA public TO rulenix_shadow_reader;
GRANT USAGE ON SCHEMA rulenix_shadow TO rulenix_shadow_writer;

SET ROLE rulenix_shadow_owner;
CREATE TABLE IF NOT EXISTS rulenix_shadow.observations (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  source_kind VARCHAR(32) NOT NULL,
  source_id TEXT NOT NULL,
  observed_at TIMESTAMPTZ NOT NULL,
  account_ref CHAR(64),
  strategy VARCHAR(64) NOT NULL,
  instrument VARCHAR(128) NOT NULL DEFAULT '',
  input_version CHAR(64) NOT NULL,
  rust_decision JSONB NOT NULL,
  python_shadow_decision JSONB NOT NULL,
  parity_classification VARCHAR(16) NOT NULL CHECK (parity_classification IN ('MATCH','MISMATCH','ERROR')),
  mismatch_reason TEXT NOT NULL DEFAULT '',
  severity VARCHAR(16) NOT NULL CHECK (severity IN ('NONE','CRITICAL','HIGH','MEDIUM','LOW')),
  rust_latency_ms DOUBLE PRECISION,
  python_latency_ms DOUBLE PRECISION NOT NULL CHECK (python_latency_ms>=0),
  failure_classification VARCHAR(64),
  source_created_at TIMESTAMPTZ,
  observer_release VARCHAR(64) NOT NULL,
  created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  UNIQUE(source_kind,source_id,observer_release)
);
CREATE INDEX IF NOT EXISTS shadow_observations_created_idx
  ON rulenix_shadow.observations(created_at DESC);
CREATE INDEX IF NOT EXISTS shadow_observations_classification_idx
  ON rulenix_shadow.observations(parity_classification,severity,created_at DESC);

CREATE TABLE IF NOT EXISTS rulenix_shadow.observer_health (
  observer_release VARCHAR(64) PRIMARY KEY,
  healthy BOOLEAN NOT NULL,
  detail TEXT NOT NULL DEFAULT '',
  last_poll_at TIMESTAMPTZ NOT NULL,
  counts JSONB NOT NULL DEFAULT '{}',
  created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
RESET ROLE;

REVOKE ALL ON ALL TABLES IN SCHEMA rulenix_shadow FROM PUBLIC;
REVOKE ALL ON ALL TABLES IN SCHEMA public FROM rulenix_shadow_reader,rulenix_shadow_writer;
REVOKE ALL ON ALL SEQUENCES IN SCHEMA public FROM rulenix_shadow_reader,rulenix_shadow_writer;

GRANT SELECT(id,strategy_key,instrument,signal_at,created_at,payload,signal_type,session_key,snapshot_id)
  ON public.strategy_signals TO rulenix_shadow_reader;
GRANT SELECT(id,signal_id,user_id,snapshot_id,role,side,lots,quantity,price)
  ON public.strategy_execution_intents TO rulenix_shadow_reader;
GRANT SELECT(id,strategy_key,instrument,contract_symbol,contract_token,lot_size,highs,lows,
  hh2,ll2,hh4,ll4,buy_entry,buy_target,buy_sl1,buy_sl2,sell_entry,sell_target,sell_sl1,
  sell_sl2,entry_direction,planned_entry,execution_key)
  ON public.strategy_market_snapshots TO rulenix_shadow_reader;
GRANT SELECT(strategy_key,instrument,event_type,created_at,payload)
  ON public.strategy_events TO rulenix_shadow_reader;
GRANT SELECT(exchange,symbol_token,interval_key,candle_time,open_price,high_price,low_price,close_price)
  ON public.backtest_market_candles TO rulenix_shadow_reader;
GRANT SELECT(user_id,healthy,checked_at,broker_credential_revision)
  ON public.broker_reconciliation_health TO rulenix_shadow_reader;
GRANT SELECT(user_id,broker_credential_revision)
  ON public.user_profiles TO rulenix_shadow_reader;
GRANT SELECT ON public.broker_deployment_account_safety TO rulenix_shadow_reader;
GRANT SELECT(user_id,rulenix_owned_exposure,ambiguous_exposure,manual_external_exposure)
  ON public.broker_reconciliation_blockers TO rulenix_shadow_reader;

GRANT INSERT,SELECT(source_kind,source_id,observer_release)
  ON rulenix_shadow.observations TO rulenix_shadow_writer;
GRANT INSERT,UPDATE,SELECT(observer_release)
  ON rulenix_shadow.observer_health TO rulenix_shadow_writer;

ALTER DEFAULT PRIVILEGES FOR ROLE rulenix_shadow_owner IN SCHEMA rulenix_shadow
  REVOKE ALL ON TABLES FROM PUBLIC;
ALTER DEFAULT PRIVILEGES FOR ROLE rulenix_shadow_owner IN SCHEMA rulenix_shadow
  REVOKE ALL ON SEQUENCES FROM PUBLIC;
