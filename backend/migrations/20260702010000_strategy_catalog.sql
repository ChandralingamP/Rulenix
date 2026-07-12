CREATE TABLE IF NOT EXISTS user_strategy_activations (
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    strategy_key VARCHAR(64) NOT NULL,
    is_active BOOLEAN NOT NULL DEFAULT FALSE,
    activated_at TIMESTAMPTZ,
    deactivated_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (user_id, strategy_key)
);

INSERT INTO user_strategy_activations (user_id, strategy_key, is_active, activated_at)
SELECT DISTINCT user_id, strategy_key, TRUE, NOW()
FROM user_strategy_configs
WHERE enabled = TRUE
ON CONFLICT (user_id, strategy_key) DO NOTHING;
