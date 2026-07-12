# Go-Live Checklist

## Staging Soak

Staging must match production infrastructure while forcing demo execution. Do not grant live trading permission in staging. Run at least two full trading days covering morning and evening sessions, restarts, feed disconnects, broker outages, and a holiday/weekend boundary when possible.

Daily report:

- Expected signals
- Simulated orders and fills
- Demo P&L
- Scheduler runs
- Alerts and delivery attempts
- Reconciliation status

Use `scripts/staging-soak-report.ps1` as a starting point.

## Production Checklist

- Secrets injected from approved secret storage.
- TLS certificate valid and auto-renewing.
- `FRONTEND_ORIGINS` matches the production HTTPS origin.
- PostgreSQL backups encrypted and restore verification passed.
- Alert webhook configured and tested.
- `/api/health/ready` and `/api/metrics` monitored.
- Risk limits reviewed.
- Global kill switch tested in demo.
- Admin and live-trading permissions reviewed.
- Angel One IP/MAC/API configuration verified.
- Incident contacts and escalation paths documented.

## Canary Live

Never place a real order during automated validation without explicit approval from the owner. Canary live requires:

- One named account with live-trading permission.
- Minimum allowed quantity/lots.
- Operator present with immediate kill-switch access.
- Broker dashboard open for independent confirmation.
- Rollback decision time set before the canary begins.

## Go/No-Go

Go only when backups, restore verification, TLS, alerting, scheduler leadership, demo soak, and risk controls are green. No-go on stale feeds, unresolved reconciliation, failed protective order tests, missing backups, or unknown broker/database disagreement.
