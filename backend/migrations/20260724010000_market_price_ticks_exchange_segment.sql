ALTER TABLE market_price_ticks
    ADD COLUMN IF NOT EXISTS exchange_segment VARCHAR(8) NOT NULL DEFAULT 'MCX';

UPDATE market_price_ticks
SET exchange_segment = 'MCX'
WHERE exchange_segment = '';

DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conname = 'market_price_ticks_pkey'
          AND conrelid = 'market_price_ticks'::regclass
          AND pg_get_constraintdef(oid) = 'PRIMARY KEY (contract_token)'
    ) THEN
        ALTER TABLE market_price_ticks DROP CONSTRAINT market_price_ticks_pkey;
    END IF;

    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conname = 'market_price_ticks_pkey'
          AND conrelid = 'market_price_ticks'::regclass
    ) THEN
        ALTER TABLE market_price_ticks
            ADD CONSTRAINT market_price_ticks_pkey PRIMARY KEY (exchange_segment, contract_token);
    END IF;
END $$;
