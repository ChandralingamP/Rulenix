# Rulenix

Rulenix is a Rust and React trading operations platform derived from the
QuantStrike user experience. It intentionally excludes the Instruments page,
Strategy Alpha, configuration, and backtesting. The retained application
includes account signup/login and OTP reset, Angel One session management,
trade P&L reporting/export, user-scoped logs, and staff administration.

## Stack

- React 19, Redux Toolkit, Vite, Tailwind CSS
- Rust 2024, Axum, Tokio, SQLx
- PostgreSQL
- Angel One SmartAPI REST login and WebSocket V2 market feed

## Project layout

```text
Rulenix/
  backend/             Rust API, migrations, Angel One clients
  frontend/            React/Redux application
  setup-database.ps1   Idempotent PostgreSQL provisioning
  start-rulenix.ps1    Local frontend/backend launcher
```

## Local setup

Prerequisites: Rust, Node.js 18+, and PostgreSQL with `psql` on `PATH`.

1. Provision the requested database and role from PowerShell. The script asks
   for a PostgreSQL administrator password and never stores it.

   ```powershell
   cd C:\Projects\Rulenix
   .\setup-database.ps1
   ```

   This creates/updates the `rulenix` role and database. The script prompts
   for the role password; use the same value in your local `DATABASE_URL`.

   - user: `rulenix`
   - database: `rulenix`

2. Start the backend once. SQLx applies `backend/migrations` automatically.

   ```powershell
   cd backend
   cargo run
   ```

3. Create the first staff administrator in a separate terminal. The password
   is read securely and is not put in shell history.

   ```powershell
   cd C:\Projects\Rulenix\backend
   cargo run -- --create-admin ADMIN admin@example.com
   ```

   If the three `INITIAL_ADMIN_*` settings are populated, the equivalent
   non-interactive one-time command is:

   ```powershell
   cargo run -- --create-admin-from-env
   ```

4. Start the frontend.

   ```powershell
   cd C:\Projects\Rulenix\frontend
   npm install
   npm run dev
   ```

Open `http://localhost:5173`. After initial setup, `start-rulenix.ps1` starts
both processes in the background and waits for a healthy proxied API. Run
`stop-rulenix.ps1` to stop them. Runtime logs and the generated initial-admin
credentials are stored under the ignored `.runlogs` directory.

If the PostgreSQL administrator password is unavailable, an elevated local
alternative is provided. It restores `pg_hba.conf` byte-for-byte after creating
the database and verifies that no temporary trust rule remains:

```powershell
Start-Process powershell -Verb RunAs -Wait -ArgumentList @(
  "-ExecutionPolicy", "Bypass", "-File",
  "C:\Projects\Rulenix\setup-database-elevated.ps1"
)
```

## Configuration

Runtime settings are in `backend/.env`. Start from the environment-specific
templates in `backend/.env.development.example`, `.env.test.example`,
`.env.staging.example`, or `.env.production.example`. Configure SMTP before
production use. In development, when SMTP is empty, OTPs are logged locally.
Public API responses never include the OTP.

### Abuse prevention

Login, signup/reset/profile OTP flows, password reset, signup, and broker
connection are rate-limited by both client IP and account identity. A rejected
request returns `429 Too Many Requests`, a `Retry-After` header, and a
`retry_after` JSON value. OTPs use keyed HMAC-SHA-256 hashes, expire, enforce a
resend cooldown and attempt ceiling, and are invalidated after use or too many
failures. Repeated login failures use configurable exponential account lockout.

Set a unique `OTP_HASH_KEY` of at least 32 random bytes in production. The
limits are documented in the `.env.*.example` templates. `TRUSTED_PROXIES` is a comma-separated
CIDR allowlist; forwarded IP headers are ignored unless the direct network peer
is in that list. Request bodies default to 64 KiB, and authentication payloads
reject unknown or malformed fields. Passwords must be 12–128 characters and
contain uppercase, lowercase, numeric, and symbol characters.

### Broker credential encryption

Angel One API keys, JWTs, refresh tokens, and feed tokens are stored in
`broker_secrets` using AES-256-GCM. Each value has its own random 96-bit nonce,
key version, ciphertext, and authentication tag. The user ID, secret kind, and
key version are authenticated as additional data, preventing ciphertext from
being moved between users or fields.

The encryption keys are required at startup and must come from the process
environment or the platform's secret manager—never source control or
PostgreSQL:

```text
CREDENTIAL_ENCRYPTION_PRIMARY_VERSION=1
CREDENTIAL_ENCRYPTION_KEYS=1:<base64-encoded 32-byte key>
```

Generate a key in PowerShell, then place the output directly into the secret
manager rather than a committed file:

```powershell
$key = [byte[]]::new(32)
[System.Security.Cryptography.RandomNumberGenerator]::Fill($key)
[Convert]::ToBase64String($key)
```

On the first startup after migration, Rulenix encrypts every non-empty legacy
plaintext credential before clearing it. It then installs a database check
constraint that prevents plaintext credentials from being written again. A
failure before clearing leaves the original value intact; a failure after the
encrypted write is safe to retry.

Rotate keys without stopping service:

1. Generate a new 32-byte key with a higher version.
2. Deploy all instances with both keys, for example
   `CREDENTIAL_ENCRYPTION_KEYS=2:<new>,1:<old>` and primary version `2`.
   Reads continue accepting version 1 while all new writes use version 2.
3. Run `cargo run -- --rotate-credentials` once. Updates are row-level and
   compare the old version, so normal reads and writes can continue.
4. Confirm `SELECT key_version, COUNT(*) FROM broker_secrets GROUP BY 1` only
   reports version 2, then remove version 1 from active instances. Retain old
   keys securely for as long as backups encrypted with them are retained.

In production, Rulenix forces PostgreSQL `verify-full` TLS regardless of the
URL's weaker setting. Configure a trusted CA, hostname-valid server
certificate, and an `sslrootcert` in `DATABASE_URL`; startup fails closed if
the database connection cannot be authenticated and encrypted.

Authentication is server-side. Login creates a database session and returns
an opaque `rulenix_session` cookie that is `HttpOnly`, `SameSite=Lax`, and
`Secure` when `APP_ENV=production`. Only the SHA-256 hash of the random
256-bit token is stored. `SESSION_IDLE_MINUTES` (default 30) and
`SESSION_ABSOLUTE_HOURS` (default 24) control expiry. Production must use HTTPS.

The separate readable `rulenix_csrf` cookie is echoed by the frontend in
`X-CSRF-Token` on POST, PUT, PATCH, and DELETE. The API compares its hash with
the session record. Logout revokes the current session; password resets revoke
all sessions for the account; role changes revoke the affected user's sessions;
expired/revoked rows are cleaned hourly. Credentialed CORS is restricted to
`FRONTEND_ORIGIN` or the comma-separated `FRONTEND_ORIGINS` allowlist.
Production origins must be HTTPS.

Production startup performs strict configuration validation for database TLS,
frontend origins, SMTP, OTP hashing, credential encryption, Angel One IP/MAC,
and required secret placeholders. See [production-security.md](docs/production-security.md).

Optionally set `INITIAL_ADMIN_USERNAME`, `INITIAL_ADMIN_EMAIL`, and
`INITIAL_ADMIN_PASSWORD`. On startup, Rulenix creates that administrator when
missing and grants administration permission without overwriting an existing password.

## Authorization and trading mode

Administration and execution mode are independent. `can_administer` controls
user and scheduler administration, `can_live_trade` allows a user to request
live mode, and `user_profiles.trading_mode` stores the selected `demo` or
`live` mode. Neither permission selects an execution mode by itself.

| Operation | Signed-in owner | `can_administer` | `can_live_trade` | Selected mode |
|---|---:|---:|---:|---:|
| Read/change own account, P&L, logs, strategies | Yes | Not required | Not required | Either |
| Top up/reset simulated balance | Yes | Not required | Not required | Demo |
| Administer users or scheduler | Yes | Required | Not relevant | Either |
| Grant/revoke live-trading permission | Yes | Required | Not relevant | Either |
| Select live mode | Yes | Not relevant | Required | Explicit confirmation and valid connected broker required |
| Submit broker orders | Yes | Not relevant | Required | Live |

Migration `20260702050000_rbac_trading_mode.sql` preserves existing staff as
administrators, but resets every existing profile to demo and grants no user
live-trading permission. This includes existing administrators. An authorized
administrator must explicitly grant `can_live_trade`; the account owner must
then explicitly confirm the switch in Account Settings. Revoking live
permission immediately returns the account to demo and revokes its sessions.

For Angel One, set the public/local IP and MAC values expected for your API
application. Users supply Client ID and API key at signup, then MPIN and TOTP
on the Home page. Tokens are stored in PostgreSQL and invalidated whenever the
API key changes.

## Market WebSocket

After connecting an Angel One session, a browser or internal client can open:

```text
ws://localhost:5173/api/ws/market?tokens=99926000&exchange_type=1&mode=1
```

The Rust bridge authenticates upstream using the saved Angel tokens, sends the
SmartAPI V2 subscription frame, parses binary little-endian packets, and emits
JSON ticks. Multiple tokens are comma-separated. `exchange_type` and `mode`
follow SmartAPI V2 values; defaults are NSE cash (`1`) and LTP (`1`).

## Futures Breakout v3

The backend runs the PDF-defined futures breakout strategy in IST. GOLDTEN is the
first instrument; additional MCX FUTCOM names use the same instrument-scoped
runtime. One daily snapshot is stored per strategy/instrument/date, so users
who connect later reuse the selected contract, four completed candles,
HH2/LL2/HH4/LL4, and all calculated levels.

Strategy activation and instrument selection are separate, matching the UI
catalogue flow. Activate the strategy first:

```http
PUT /api/strategies/futures_breakout_v3/activation
Content-Type: application/json

{"active":true}
```

Then enable GOLDTEN and set its integer lot count:

```http
PUT /api/strategy/futures-breakout
Content-Type: application/json

{
  "instrument": "GOLDTEN",
  "enabled": true,
  "lots": 3,
  "run_day_session": true,
  "run_evening_session": true
}
```

`GET /api/strategies` returns the authenticated user's strategy catalogue and
available instruments. `GET /api/strategy/futures-breakout?instrument=GOLDTEN`
returns the shared snapshot plus the user's orders and trades. Live events are available
at `ws://localhost:5173/api/ws/strategy`. Both WebSockets authenticate from the
session cookie during their HTTP upgrade. One shared MCX token
feed services all demo users; live users submit SmartAPI broker orders and
reconcile broker fills. Every position is also written to `trades`.

The scheduler persists each day/session/action in `strategy_scheduler_runs`.
After a restart it safely catches up the 09:00/09:10 and 17:00/17:10 IST work
within a bounded 15-minute window, retries transient failures every 30 seconds,
and never submits a new entry after that window. Protective TARGET/SL orders
have a separate recovery path and continue retrying after broker reconnection.
Shared market feeds reconnect with backoff, while one user's reconciliation
failure does not block other live users.

Every demo and live order is reserved through the database-backed risk engine.
Entry checks are serialized per user and cover lots, quantity, notional,
positions, daily trades, realized/unrealized loss, current snapshots, fresh
market ticks, account/session health, broker reconciliation, and live margin.
Each allow or rejection is stored in `risk_decisions` with its measured values.
Global and per-user kill switches atomically stop new entries and cancel pending
entries without cancelling TARGET, SL1, or SL2 protective exits. Staff can edit
global/per-user limits and operate both kill-switch scopes in the Admin Console.

Live Angel One submissions use a stable `ordertag` and a durable order state
machine (`pending`, `submitting`, `ambiguous`, `submitted`, `partially_filled`,
`processing`, and terminal states). Nullable Angel envelopes are decoded
correctly; malformed replies retain only bounded, redacted HTTP diagnostics.
Ambiguous submissions are reconciled by broker order ID or tag and are never
blindly retried. Reconciliation records cumulative quantities and average fill
prices, freezes partial fills before applying the confirmed quantity, and
recovers interrupted submissions/fill processing after restart. Each user is
reconciled in an independent task. Shared feeds detect stale ticks and reconnect
with exponential backoff and jitter.

Session closures are controlled by `market_calendar`. The migration seeds the
official session-specific MCX 2026 holidays; update this table when MCX
publishes a new annual calendar or a special-session circular. Weekends are
closed automatically unless an explicit calendar row overrides them. Scheduler
runs and deduplicated operational alerts appear on the Strategies page.

Angel One sessions are maintained in the background once per minute. JWT and
feed tokens are renewed through SmartAPI `generateTokens` when the JWT is
within ten minutes of expiry. Rejected refresh tokens are cleared and reported
as invalid; temporary verification failures are reported without discarding
otherwise reusable credentials.

## Primary API routes

- `POST /api/auth/request-otp/`, `/signup/`, `/login/`
- `GET /api/auth/access/`, `POST /api/auth/logout/`
- `POST /api/auth/password/request-reset/`, `/verify-otp/`, `/reset/`
- `GET|PATCH|DELETE /api/auth/admin/users/`
- `GET /api/home/status/`, `POST /api/home/connect/`, `PATCH /api/home/profile/`
- `GET|PATCH /api/account/profile`, `POST /api/account/profile/request-otp`
- `GET /api/account/balance`, `POST /api/account/balance/top-up`, `/reset`
- `PUT /api/account/trading-mode`
- `GET /api/pnl`, `/api/pnl/export`
- `GET /api/logs/files/`, `/api/logs/content/`
- `GET /api/scheduler/jobs/`, `POST /api/scheduler/trigger/`
- `GET /api/risk/admin`, `PUT /api/risk/admin/limits[/{user_id}]`
- `PUT /api/risk/admin/kill-switch[/{user_id}]`
- `GET /api/ws/market` (WebSocket upgrade)
- `GET /api/strategies`, `PUT /api/strategies/{strategy_key}/activation`
- `GET|PUT /api/strategy/futures-breakout`
- `GET /api/ws/strategy` (WebSocket upgrade)

The header gear opens account settings. Username, email, mobile number, and
Angel One Client ID changes are verified against an OTP sent to the current
email address. Live accounts load available margin from Angel One `getRMS`;
demo accounts start with ₹2,00,000 and support local top-up/reset controls.
Demo orders are simulated from the shared live market feed and persisted in
`strategy_orders`; fills create/update `trades`, realized P&L adjusts the demo
balance, and submissions/fills/exits are written to the user's activity log.

Every account, balance, P&L, log, strategy, scheduler, administration, market
WebSocket, and strategy WebSocket route requires a valid session. Identity and
roles come only from that session. Protected schemas reject legacy `username`
and `admin_username` identity fields.

## Verification

```powershell
cd backend
cargo test

cd ..\frontend
npm run lint
npm test -- --run
npm run build
```

## Production operations

- Deployment and rollback: [deployment.md](docs/deployment.md)
- TLS and secret injection: [production-security.md](docs/production-security.md)
- Observability and audit trail: [observability.md](docs/observability.md)
- Backup and disaster recovery: [disaster-recovery.md](docs/disaster-recovery.md)
- Staging soak and go-live: [go-live-checklist.md](docs/go-live-checklist.md)
