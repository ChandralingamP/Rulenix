# Deployment

## Docker

1. Create `backend/.env.production` from `backend/.env.production.example` and inject real secrets outside source control.
2. Create `secrets/postgres_password.txt` with the PostgreSQL password.
3. Build and start:

```powershell
docker compose -f docker-compose.prod.yml up --build -d
```

Migrations run in the backend at startup behind a PostgreSQL advisory lock. If a migration fails, backend startup fails and deployment should be considered failed.

## Non-Docker Service

Build:

```powershell
cd backend
cargo build --release --locked
cd ..\frontend
npm ci
npm run build
```

Run the Rust binary under a service manager such as systemd or Windows Service Wrapper. Serve `frontend/dist` through Nginx or Caddy and proxy `/api` and WebSocket upgrades to the backend. Do not use Vite in production.

See `infra/systemd/rulenix-backend.service` and `infra/nginx/rulenix.conf`.

## Upgrade

1. Verify backups and restore verification are current.
2. Deploy backend first so migrations run exactly once.
3. Wait for `/api/health/ready`.
4. Deploy frontend static bundle.
5. Review `/api/metrics`, logs, and alert delivery attempts.

## Rollback

1. Stop new traffic at the reverse proxy.
2. Roll back the frontend bundle.
3. Roll back the backend binary only if the database migration is backward-compatible.
4. If the migration is not backward-compatible, follow `docs/disaster-recovery.md` forward-recovery or restore procedures.

## Restart

Use graceful service stop. The HTTP server drains with the process signal. Scheduler leadership is released when the backend database connection closes; another replica can acquire it.
