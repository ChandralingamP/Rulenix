Phase 2 deliberately contains no schema migration. The Rust migration ledger remains authoritative
(49 successful migrations through 20260904010000). Alembic is adoption-only and must not upgrade
production; future migrations require an explicitly reviewed Phase 3 change.

