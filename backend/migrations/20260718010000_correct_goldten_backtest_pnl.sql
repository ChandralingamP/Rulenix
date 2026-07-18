-- Futures Breakout v3 originally treated one configured GOLDTEN lot as one
-- price unit. Correct saved results to use the broker contract lot size.
UPDATE backtest_trades AS trade
SET quantity = trade.lots * run.lot_size,
    realized_pnl = trade.realized_pnl * run.lot_size
FROM backtest_runs AS run
WHERE trade.run_id = run.id
  AND run.strategy_key = 'futures_breakout_v3'
  AND run.instrument = 'GOLDTEN'
  AND run.lot_size <> 1
  AND COALESCE((run.summary ->> 'pnl_multiplier_per_lot')::DOUBLE PRECISION, 1) = 1;

UPDATE backtest_runs
SET summary = summary || jsonb_build_object(
    'net_pnl', COALESCE((summary ->> 'net_pnl')::DOUBLE PRECISION, 0) * lot_size,
    'gross_profit', COALESCE((summary ->> 'gross_profit')::DOUBLE PRECISION, 0) * lot_size,
    'gross_loss', COALESCE((summary ->> 'gross_loss')::DOUBLE PRECISION, 0) * lot_size,
    'average_pnl', COALESCE((summary ->> 'average_pnl')::DOUBLE PRECISION, 0) * lot_size,
    'average_win', COALESCE((summary ->> 'average_win')::DOUBLE PRECISION, 0) * lot_size,
    'average_loss', COALESCE((summary ->> 'average_loss')::DOUBLE PRECISION, 0) * lot_size,
    'max_drawdown', COALESCE((summary ->> 'max_drawdown')::DOUBLE PRECISION, 0) * lot_size,
    'pnl_multiplier_per_lot', lot_size
)
WHERE strategy_key = 'futures_breakout_v3'
  AND instrument = 'GOLDTEN'
  AND lot_size <> 1
  AND COALESCE((summary ->> 'pnl_multiplier_per_lot')::DOUBLE PRECISION, 1) = 1;
