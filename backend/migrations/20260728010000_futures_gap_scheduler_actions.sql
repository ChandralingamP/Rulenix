ALTER TABLE strategy_scheduler_runs
    DROP CONSTRAINT IF EXISTS strategy_scheduler_runs_action_check;

ALTER TABLE strategy_scheduler_runs
    ADD CONSTRAINT strategy_scheduler_runs_action_check
    CHECK (action IN ('target', 'stop', 'entry', 'gap_entry'));
