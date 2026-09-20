\set ON_ERROR_STOP on

\if :{?demo_reader_password}
\else
  \echo 'demo_reader_password is required'
  \quit 2
\endif
\if :{?demo_writer_password}
\else
  \echo 'demo_writer_password is required'
  \quit 2
\endif

DO $$
BEGIN
  IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname='rulenix_demo_trial_owner') THEN
    CREATE ROLE rulenix_demo_trial_owner NOLOGIN NOINHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;
  END IF;
  IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname='rulenix_demo_trial_reader') THEN
    CREATE ROLE rulenix_demo_trial_reader LOGIN NOINHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;
  END IF;
  IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname='rulenix_demo_trial_writer') THEN
    CREATE ROLE rulenix_demo_trial_writer LOGIN NOINHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;
  END IF;
END
$$;

ALTER ROLE rulenix_demo_trial_reader PASSWORD :'demo_reader_password';
ALTER ROLE rulenix_demo_trial_writer PASSWORD :'demo_writer_password';
ALTER ROLE rulenix_demo_trial_reader CONNECTION LIMIT 3;
ALTER ROLE rulenix_demo_trial_writer CONNECTION LIMIT 3;
ALTER ROLE rulenix_demo_trial_reader SET default_transaction_read_only=on;
ALTER ROLE rulenix_demo_trial_reader SET statement_timeout='5s';
ALTER ROLE rulenix_demo_trial_reader SET lock_timeout='1s';
ALTER ROLE rulenix_demo_trial_writer SET statement_timeout='5s';
ALTER ROLE rulenix_demo_trial_writer SET lock_timeout='1s';

SELECT format(
  'GRANT CONNECT ON DATABASE %I TO rulenix_demo_trial_reader,rulenix_demo_trial_writer',
  current_database()
) \gexec
CREATE SCHEMA IF NOT EXISTS rulenix_demo_trial AUTHORIZATION rulenix_demo_trial_owner;
ALTER SCHEMA rulenix_demo_trial OWNER TO rulenix_demo_trial_owner;
REVOKE ALL ON SCHEMA rulenix_demo_trial FROM PUBLIC;
GRANT USAGE ON SCHEMA public TO rulenix_demo_trial_reader;
GRANT USAGE ON SCHEMA rulenix_demo_trial TO rulenix_demo_trial_writer;

SET ROLE rulenix_demo_trial_owner;
CREATE TABLE IF NOT EXISTS rulenix_demo_trial.assignments (
  account_ref VARCHAR(64) NOT NULL,
  strategy_key VARCHAR(64) NOT NULL,
  execution_mode VARCHAR(8) NOT NULL CHECK(execution_mode='demo'),
  active BOOLEAN NOT NULL DEFAULT TRUE,
  created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  PRIMARY KEY(account_ref,strategy_key)
);
CREATE TABLE IF NOT EXISTS rulenix_demo_trial.cycles (
  id UUID PRIMARY KEY,
  cycle_key VARCHAR(192) NOT NULL UNIQUE,
  source_kind VARCHAR(64) NOT NULL,
  account_ref VARCHAR(64) NOT NULL,
  strategy_key VARCHAR(64) NOT NULL,
  instrument VARCHAR(128) NOT NULL,
  scheduled_for TIMESTAMPTZ NOT NULL,
  status VARCHAR(16) NOT NULL CHECK(status IN ('pending','running','completed','failed')),
  attempts INTEGER NOT NULL DEFAULT 0 CHECK(attempts>=0),
  claimed_at TIMESTAMPTZ,
  completed_at TIMESTAMPTZ,
  oracle JSONB NOT NULL DEFAULT '{}',
  decision JSONB NOT NULL DEFAULT '{}',
  parity_classification VARCHAR(16) CHECK(parity_classification IN ('MATCH','MISMATCH','ERROR')),
  mismatch_reason TEXT NOT NULL DEFAULT '',
  last_error TEXT NOT NULL DEFAULT '',
  observer_release VARCHAR(80) NOT NULL,
  created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS demo_trial_cycles_status_idx
  ON rulenix_demo_trial.cycles(status,updated_at);
CREATE TABLE IF NOT EXISTS rulenix_demo_trial.signals (
  id UUID PRIMARY KEY,
  cycle_id UUID NOT NULL UNIQUE REFERENCES rulenix_demo_trial.cycles(id),
  signal_type VARCHAR(32) NOT NULL,
  side VARCHAR(8) NOT NULL CHECK(side IN ('BUY','SELL')),
  price NUMERIC(20,8) NOT NULL,
  created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE TABLE IF NOT EXISTS rulenix_demo_trial.intents (
  id UUID PRIMARY KEY,
  cycle_id UUID NOT NULL REFERENCES rulenix_demo_trial.cycles(id),
  role VARCHAR(32) NOT NULL,
  side VARCHAR(8) NOT NULL CHECK(side IN ('BUY','SELL')),
  quantity INTEGER NOT NULL CHECK(quantity>0),
  price NUMERIC(20,8) NOT NULL,
  status VARCHAR(16) NOT NULL CHECK(status IN ('pending','claimed','completed','failed')),
  idempotency_key VARCHAR(255) NOT NULL UNIQUE,
  created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE TABLE IF NOT EXISTS rulenix_demo_trial.orders (
  id UUID PRIMARY KEY,
  cycle_id UUID NOT NULL REFERENCES rulenix_demo_trial.cycles(id),
  role VARCHAR(32) NOT NULL,
  side VARCHAR(8) NOT NULL CHECK(side IN ('BUY','SELL')),
  quantity INTEGER NOT NULL CHECK(quantity>0),
  price NUMERIC(20,8) NOT NULL,
  status VARCHAR(16) NOT NULL CHECK(status IN ('filled','cancelled')),
  idempotency_key VARCHAR(255) NOT NULL UNIQUE,
  created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE TABLE IF NOT EXISTS rulenix_demo_trial.trades (
  id UUID PRIMARY KEY,
  cycle_id UUID NOT NULL REFERENCES rulenix_demo_trial.cycles(id),
  lineage VARCHAR(16) NOT NULL CHECK(lineage IN ('source','reversal')),
  status VARCHAR(16) NOT NULL CHECK(status='closed'),
  side VARCHAR(8) NOT NULL CHECK(side IN ('BUY','SELL')),
  quantity INTEGER NOT NULL CHECK(quantity>0),
  entry_price NUMERIC(20,8) NOT NULL,
  exit_price NUMERIC(20,8) NOT NULL,
  exit_reason VARCHAR(32) NOT NULL,
  pnl NUMERIC(20,2) NOT NULL,
  created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  UNIQUE(cycle_id,lineage)
);
CREATE TABLE IF NOT EXISTS rulenix_demo_trial.events (
  id UUID PRIMARY KEY,
  cycle_id UUID NOT NULL REFERENCES rulenix_demo_trial.cycles(id),
  event_type VARCHAR(32) NOT NULL,
  payload JSONB NOT NULL,
  created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  UNIQUE(cycle_id,event_type)
);
CREATE TABLE IF NOT EXISTS rulenix_demo_trial.health (
  observer_release VARCHAR(80) PRIMARY KEY,
  healthy BOOLEAN NOT NULL,
  detail TEXT NOT NULL DEFAULT '',
  last_poll_at TIMESTAMPTZ NOT NULL,
  counts JSONB NOT NULL DEFAULT '{}',
  created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
RESET ROLE;

REVOKE ALL ON ALL TABLES IN SCHEMA public FROM rulenix_demo_trial_reader,rulenix_demo_trial_writer;
REVOKE ALL ON ALL SEQUENCES IN SCHEMA public FROM rulenix_demo_trial_reader,rulenix_demo_trial_writer;
REVOKE ALL ON ALL TABLES IN SCHEMA rulenix_demo_trial FROM PUBLIC;
GRANT SELECT(trade_date,morning_open,evening_open,reason)
  ON public.market_calendar TO rulenix_demo_trial_reader;
GRANT SELECT(strategy_key,trade_date,status,last_error)
  ON public.strategy_scheduler_runs TO rulenix_demo_trial_reader;
GRANT SELECT(strategy_key,signal_at,signal_type,expected_users)
  ON public.strategy_signals TO rulenix_demo_trial_reader;
GRANT SELECT(user_id,enabled)
  ON public.risk_kill_switches TO rulenix_demo_trial_reader;
GRANT INSERT,UPDATE,SELECT ON ALL TABLES IN SCHEMA rulenix_demo_trial TO rulenix_demo_trial_writer;

ALTER DEFAULT PRIVILEGES FOR ROLE rulenix_demo_trial_owner IN SCHEMA rulenix_demo_trial
  REVOKE ALL ON TABLES FROM PUBLIC;
