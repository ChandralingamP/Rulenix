CREATE TEMP TABLE goldten_backtest_pnl_fix ON COMMIT DROP AS
SELECT id AS run_id,
       lot_size::DOUBLE PRECISION AS multiplier
FROM backtest_runs
WHERE instrument = 'GOLDTEN'
  AND lot_size > 1
  AND COALESCE((summary ->> 'pnl_multiplier_per_lot')::DOUBLE PRECISION, 1) = lot_size::DOUBLE PRECISION;

UPDATE backtest_trades trade
SET realized_pnl = trade.realized_pnl / fix.multiplier,
    levels = trade.levels || jsonb_build_object(
        'partial_realized_pnl',
        COALESCE((trade.levels ->> 'partial_realized_pnl')::DOUBLE PRECISION, 0) / fix.multiplier,
        'final_leg_pnl',
        COALESCE((trade.levels ->> 'final_leg_pnl')::DOUBLE PRECISION, 0) / fix.multiplier,
        'calculated_pnl',
        COALESCE((trade.levels ->> 'calculated_pnl')::DOUBLE PRECISION, trade.realized_pnl) / fix.multiplier,
        'pnl_model',
        'goldten_points_x_lots'
    )
FROM goldten_backtest_pnl_fix fix
WHERE trade.run_id = fix.run_id;

UPDATE backtest_runs run
SET summary = run.summary || jsonb_build_object(
        'net_pnl',
        COALESCE((run.summary ->> 'net_pnl')::DOUBLE PRECISION, 0) / fix.multiplier,
        'gross_profit',
        COALESCE((run.summary ->> 'gross_profit')::DOUBLE PRECISION, 0) / fix.multiplier,
        'gross_loss',
        COALESCE((run.summary ->> 'gross_loss')::DOUBLE PRECISION, 0) / fix.multiplier,
        'average_pnl',
        COALESCE((run.summary ->> 'average_pnl')::DOUBLE PRECISION, 0) / fix.multiplier,
        'average_win',
        COALESCE((run.summary ->> 'average_win')::DOUBLE PRECISION, 0) / fix.multiplier,
        'average_loss',
        COALESCE((run.summary ->> 'average_loss')::DOUBLE PRECISION, 0) / fix.multiplier,
        'max_drawdown',
        COALESCE((run.summary ->> 'max_drawdown')::DOUBLE PRECISION, 0) / fix.multiplier,
        'pnl_multiplier_per_lot',
        1,
        'pnl_model',
        'goldten_points_x_lots'
    )
FROM goldten_backtest_pnl_fix fix
WHERE run.id = fix.run_id;
