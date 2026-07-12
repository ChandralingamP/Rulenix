# Database Backup And Disaster Recovery

## Objectives

Initial targets:

- RPO: 15 minutes when WAL archiving is enabled; otherwise the last successful encrypted dump.
- RTO: 60 minutes for a tested restore to a warm PostgreSQL host.

Adjust these targets after the first production restore drill.

## Backups

Use `scripts/backup-postgres.ps1` from a trusted host with `pg_dump` and `openssl`:

```powershell
.\scripts\backup-postgres.ps1 `
  -DatabaseUrl $env:DATABASE_URL `
  -OutputDirectory D:\rulenix-backups `
  -EncryptionPassphraseFile D:\secrets\rulenix-backup-passphrase.txt `
  -RetentionDays 14
```

Backups are encrypted with AES-256-CBC and PBKDF2. Store the passphrase in a secret manager. Do not copy application encryption keys into backup archives.

Back up non-secret configuration metadata separately: image tags, environment variable names, reverse-proxy config, migration version, and scheduler settings. Do not copy secret values.

## Restore Verification

Run restore verification into an isolated database:

```powershell
.\scripts\restore-verify-postgres.ps1 `
  -BackupFile D:\rulenix-backups\rulenix-YYYYMMDD-HHMMSS.dump.enc `
  -MaintenanceDatabaseUrl postgres://admin:secret@db-host:5432/postgres `
  -VerificationDatabase rulenix_restore_verify `
  -EncryptionPassphraseFile D:\secrets\rulenix-backup-passphrase.txt
```

The script restores, runs a basic table check, and drops the verification database.

## Point-In-Time Recovery

For PITR, configure PostgreSQL WAL archiving to encrypted object storage and test recovery to a named timestamp. Keep base backups and WAL for at least the configured RPO/RTO window plus compliance retention.

## Migration Recovery

Prefer forward recovery: create a corrective migration and redeploy. Use restore only when data corruption or an incompatible migration cannot be fixed safely.

## Trading State Recovery

- Corrupted order state: freeze live mode, enable global kill switch, reconcile broker order book, then correct local state through an audited migration.
- Lost scheduler state: scheduler runs are durable; restart backend and verify pending/failed runs. The leader lock prevents duplicate execution.
- Broker/database disagreement: keep live mode disabled for affected users until broker order book, local `strategy_orders`, and `trades` match.

## Data Retention And Export

Define account deletion and user export through audited admin workflows before broad production onboarding. Export should include profile, sessions, trades, P&L, strategy events, and audit records relevant to the user, excluding secrets.
