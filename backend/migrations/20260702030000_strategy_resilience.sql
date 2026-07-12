CREATE TABLE IF NOT EXISTS strategy_scheduler_runs (
    id UUID PRIMARY KEY,
    strategy_key VARCHAR(64) NOT NULL,
    instrument VARCHAR(32) NOT NULL,
    trade_date DATE NOT NULL,
    session_key VARCHAR(16) NOT NULL CHECK (session_key IN ('day', 'evening')),
    action VARCHAR(16) NOT NULL CHECK (action IN ('target', 'entry')),
    status VARCHAR(16) NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'running', 'completed', 'failed', 'skipped')),
    attempts INTEGER NOT NULL DEFAULT 0,
    scheduled_for TIMESTAMPTZ NOT NULL,
    next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    started_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    last_error TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (strategy_key, instrument, trade_date, session_key, action)
);
CREATE INDEX IF NOT EXISTS strategy_scheduler_runs_due_idx
    ON strategy_scheduler_runs (status, next_attempt_at);

CREATE TABLE IF NOT EXISTS market_calendar (
    trade_date DATE PRIMARY KEY,
    morning_open BOOLEAN NOT NULL DEFAULT TRUE,
    evening_open BOOLEAN NOT NULL DEFAULT TRUE,
    reason VARCHAR(128) NOT NULL DEFAULT '',
    source VARCHAR(255) NOT NULL DEFAULT '',
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- MCX 2026 trading holidays. Session-specific closures matter because several
-- holidays close only the morning or evening commodity session.
INSERT INTO market_calendar (trade_date, morning_open, evening_open, reason, source) VALUES
('2026-01-01', TRUE,  FALSE, 'New Year Day', 'MCX trading holidays 2026'),
('2026-01-26', FALSE, FALSE, 'Republic Day', 'MCX trading holidays 2026'),
('2026-03-03', FALSE, TRUE,  'Holi', 'MCX trading holidays 2026'),
('2026-03-26', FALSE, TRUE,  'Shri Ram Navmi', 'MCX trading holidays 2026'),
('2026-03-31', FALSE, TRUE,  'Shri Mahavir Jayanti', 'MCX trading holidays 2026'),
('2026-04-03', FALSE, FALSE, 'Good Friday', 'MCX trading holidays 2026'),
('2026-04-14', FALSE, TRUE,  'Dr. Baba Saheb Ambedkar Jayanti', 'MCX trading holidays 2026'),
('2026-05-01', FALSE, TRUE,  'Maharashtra Day', 'MCX trading holidays 2026'),
('2026-05-28', FALSE, TRUE,  'Bakri Id', 'MCX trading holidays 2026'),
('2026-06-26', FALSE, TRUE,  'Moharram', 'MCX trading holidays 2026'),
('2026-09-14', FALSE, TRUE,  'Ganesh Chaturthi', 'MCX trading holidays 2026'),
('2026-10-02', FALSE, FALSE, 'Mahatma Gandhi Jayanti', 'MCX trading holidays 2026'),
('2026-10-20', FALSE, TRUE,  'Dassera', 'MCX trading holidays 2026'),
('2026-11-10', FALSE, TRUE,  'Diwali-Balipratipada', 'MCX trading holidays 2026'),
('2026-11-24', FALSE, TRUE,  'Guru Nanak Jayanti', 'MCX trading holidays 2026'),
('2026-12-25', FALSE, FALSE, 'Christmas', 'MCX trading holidays 2026')
ON CONFLICT (trade_date) DO NOTHING;
