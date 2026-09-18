ALTER TABLE trades
    ADD COLUMN IF NOT EXISTS last_exact_broker_exposure_at TIMESTAMPTZ;

UPDATE trades AS trade
SET last_exact_broker_exposure_at = trade.last_position_reconciled_at
FROM strategy_market_snapshots AS snapshot
WHERE snapshot.id = trade.strategy_snapshot_id
  AND trade.execution_mode = 'live'
  AND trade.status = 'open'
  AND trade.last_exact_broker_exposure_at IS NULL
  AND trade.last_position_reconciled_at IS NOT NULL
  AND trade.broker_net_quantity = CASE WHEN trade.direction = 'BUY' THEN trade.quantity ELSE -trade.quantity END
  AND NOT EXISTS (
      SELECT 1
      FROM trades AS other_trade
      JOIN strategy_market_snapshots AS other_snapshot
        ON other_snapshot.id = other_trade.strategy_snapshot_id
      WHERE other_trade.id <> trade.id
        AND other_trade.user_id = trade.user_id
        AND other_trade.execution_mode = 'live'
        AND other_trade.status = 'open'
        AND UPPER(other_snapshot.exchange_segment) = UPPER(snapshot.exchange_segment)
        AND other_snapshot.contract_token = snapshot.contract_token
  );

CREATE TABLE IF NOT EXISTS manual_broker_close_evidence (
    trade_id UUID PRIMARY KEY REFERENCES trades(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    broker_credential_revision BIGINT NOT NULL,
    exchange_segment VARCHAR(16) NOT NULL,
    contract_token VARCHAR(32) NOT NULL,
    contract_symbol VARCHAR(96) NOT NULL DEFAULT '',
    close_side VARCHAR(4) NOT NULL CHECK (close_side IN ('BUY', 'SELL')),
    filled_quantity INTEGER NOT NULL CHECK (filled_quantity > 0),
    weighted_fill_price NUMERIC(20, 6) NOT NULL CHECK (weighted_fill_price > 0),
    broker_order_ids TEXT[] NOT NULL CHECK (cardinality(broker_order_ids) > 0),
    first_fill_at TIMESTAMPTZ NOT NULL,
    last_fill_at TIMESTAMPTZ NOT NULL,
    observed_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    consumed_at TIMESTAMPTZ,
    CHECK (last_fill_at >= first_fill_at)
);

CREATE INDEX IF NOT EXISTS manual_broker_close_evidence_pending_idx
    ON manual_broker_close_evidence (user_id, observed_at)
    WHERE consumed_at IS NULL;

COMMENT ON COLUMN trades.last_exact_broker_exposure_at IS
    'Latest authoritative reconciliation at which broker net exposure exactly matched this sole open LIVE trade.';

COMMENT ON TABLE manual_broker_close_evidence IS
    'Durable exact external-fill evidence scoped to one broker credential revision and retained while protective-order cleanup may require later reconciliation passes.';
