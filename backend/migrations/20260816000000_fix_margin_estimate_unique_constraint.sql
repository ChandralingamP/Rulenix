WITH ranked AS (
    SELECT
        id,
        ROW_NUMBER() OVER (
            PARTITION BY exchange, symbol_token, product_type, order_type, trade_type, lot_size
            ORDER BY fetched_at DESC, id DESC
        ) AS row_number
    FROM broker_margin_estimates
)
DELETE FROM broker_margin_estimates estimates
USING ranked
WHERE estimates.id = ranked.id
  AND ranked.row_number > 1;

ALTER TABLE broker_margin_estimates
    DROP CONSTRAINT IF EXISTS broker_margin_estimates_exchange_symbol_token_product_type_trade_t_key;

ALTER TABLE broker_margin_estimates
    DROP CONSTRAINT IF EXISTS broker_margin_estimates_exchange_symbol_token_product_type__key;

DO $$
DECLARE
    old_constraint record;
BEGIN
    FOR old_constraint IN
        SELECT constraint_name
        FROM information_schema.table_constraints
        WHERE table_schema = 'public'
          AND table_name = 'broker_margin_estimates'
          AND constraint_type = 'UNIQUE'
          AND constraint_name <> 'broker_margin_estimates_unique_order_type_idx'
    LOOP
        IF (
            SELECT array_agg(column_name::text ORDER BY ordinal_position)
            FROM information_schema.key_column_usage
            WHERE table_schema = 'public'
              AND table_name = 'broker_margin_estimates'
              AND constraint_name = old_constraint.constraint_name
        ) = ARRAY['exchange','symbol_token','product_type','trade_type','lot_size'] THEN
            EXECUTE format(
                'ALTER TABLE broker_margin_estimates DROP CONSTRAINT IF EXISTS %I',
                old_constraint.constraint_name
            );
        END IF;
    END LOOP;
END $$;

CREATE UNIQUE INDEX IF NOT EXISTS broker_margin_estimates_unique_order_type_idx
    ON broker_margin_estimates (exchange, symbol_token, product_type, order_type, trade_type, lot_size);
