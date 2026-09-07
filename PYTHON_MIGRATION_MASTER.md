# Rulenix Rust to Python Migration Master Specification

> Phase 1 production audit and behavioral migration contract. This document is specification only: it does not authorize a Python implementation, deployment, database change, broker mutation, network change, or production operation.

## 0. Authority, scope, and evidence

> Historical audit baseline. Section 36 is the authoritative current-production reconciliation and resolves/supersedes the provenance and release facts in this section.

The authoritative production source release named by the migration brief is commit `09a03a1d2267773afac4b719f4ab1ab239588aaf` (`angel-egress-release`). The local checked-out branch during this audit was `master` at `e75681792584c8e0eee61ecf576247e9d8771005`. The working tree already contained many unrelated modified and untracked files. Production-specific files were therefore read directly from the production Git object with `git show`; they were not inferred from stale untracked copies and no checkout was performed.

Evidence labels used here:

- **SOURCE-VERIFIED**: traced in the production release or in a clearly identified current source file.
- **TEST-VERIFIED**: exercised by a safe local repository test in this audit.
- **REPORTED**: stated by a deployment report or the production brief, but not independently checked against a server.
- **GAP/BLOCKER**: the required production artifact or proof is absent.

Repository inspection proves the production egress code and migration exist at `09a03a1…`. It does **not** prove the live server is currently running that object. No SSH, production database, Angel session, helper, network, or service command was used. The reported runtime baseline is:

- PostgreSQL, backend, frontend, internal readiness, and public readiness healthy.
- Schema migration `20260826000000`; 47 successful and 0 failed.
- Default outbound public address `139.99.155.62`; additional address `51.161.140.103`.
- Both addresses `CONFIGURED / VERIFIED / AVAILABLE`; zero explicit assignments; NULL/default semantics active.
- Restricted helper active; startup rehydration passed; host default route unchanged.
- Latest deployment placed, modified, and cancelled zero live orders.

These runtime statements remain **REPORTED**, not source-verifiable facts.

### Critical provenance finding

The brief says production has a narrow classifier for an Angel `OCO_LIMIT` informational record for `NIFTY01SEP2624100CE`. The repository contains the investigation and claimed 10-test result only in the untracked `ANGEL_EGRESS_DEPLOYMENT_BLOCKED_REPORT.md`. Neither production commit `09a03a1…`, checked-out backend source, test source, nor Git history contains the classifier, individual-order API, trade-book API, conditional/GTT inventory API, `AB1007` handling, or the ten tests. Consequently:

1. the strict conjunction is recorded below as a required fail-closed contract derived from operational evidence;
2. it must not be reimplemented from prose alone;
3. Phase 2 is blocked until the exact deployed classifier source, broker request/response fixtures, and its test suite are made repository-authoritative.

## 1. Repository map

```text
Rulenix/
├── .cargo/config.toml                 Cargo linker/build configuration
├── .github/workflows/ci.yml           Rust, frontend, audit, and secret CI
├── backend/
│   ├── Cargo.toml / Cargo.lock         Single Rust crate: rulenix-backend
│   ├── Dockerfile                     Production backend image
│   ├── .env.example                   Backend configuration contract
│   ├── migrations/                    47 ordered PostgreSQL migrations
│   └── src/
│       ├── main.rs                     startup, routing, state, middleware, tasks
│       ├── config.rs                   environment parsing and production checks
│       ├── auth.rs                     OTP, login, sessions, users/admin
│       ├── security.rs                 passwords, OTP/token hashing, rate limiting
│       ├── credentials.rs              AES-GCM broker secret store and rotation
│       ├── models.rs                   shared application/domain records
│       ├── angel.rs                    Angel REST client and write classification
│       ├── egress.rs                   per-user egress selection and bound clients
│       ├── market_ws.rs                browser-to-Angel and shared market feeds
│       ├── strategy.rs                 strategies, execution, protection, recovery,
│       │                                reconciliation, scheduling, WebSocket events
│       ├── risk.rs                     limits, kill switches, reservations
│       ├── backtesting.rs              futures-breakout backtest and exports
│       ├── account.rs                  profile and demo/live mode
│       ├── home.rs                     broker connect and dashboard state
│       ├── pnl.rs                      trades/P&L list and XLSX export
│       ├── jobs.rs                     administrator maintenance jobs
│       ├── logs.rs                     bounded per-user/admin log access
│       ├── audit.rs                    immutable audit and alert delivery
│       ├── error.rs                    public error envelope
│       └── bin/rulenix-egress-helper.rs privileged narrow host helper
├── frontend/
│   ├── package.json / vite.config.js   React/Vite application and tooling
│   ├── Dockerfile / nginx.conf         static image and SPA/API proxy
│   └── src/                            pages, API client, auth context, tests
├── infra/
│   ├── caddy/Caddyfile                 public TLS/reverse proxy
│   ├── nginx/rulenix.conf              alternative reverse proxy
│   └── systemd/                        backend and root egress-helper units
├── scripts/                            deploy/backup/restore/operational scripts
├── tests/load/                          readiness/load smoke tooling
├── docs/angel-egress.md                egress operations and security design
├── docker-compose.prod.yml             PostgreSQL/backend/frontend services
└── *REPORT.md / README.md               release, remediation, and audit evidence
```

This is one Cargo package, not a multi-crate workspace. Production source size is approximately 25,853 lines across 26 Rust files; `strategy.rs` alone is approximately 13,731 lines and is the principal migration concentration risk.

### Significant Rust modules

| Path | Purpose and major entry points/types | Important dependencies | Safety relevance |
|---|---|---|---|
| `backend/src/main.rs` | `main`, router construction, `AppState`, migrations, graceful signal, worker startup | Axum, Tokio, SQLx, tower-http | CRITICAL: activates every runtime capability |
| `config.rs` | `Config::from_env`, production validation | environment, URL/IP parsing | HIGH: fail-fast security and mode boundaries |
| `auth.rs` | request/signup/login/reset/logout, session middleware, admin APIs, session maintenance | SQLx, security, credentials, Angel | HIGH: identity, permission, session and broker-account boundary |
| `security.rs` | Argon2, HMAC OTP, token hashing, password policy, rate gates | argon2, HMAC/SHA-256, RNG | HIGH |
| `credentials.rs` | `CredentialStore`, encrypt/decrypt, plaintext migration, key rotation | AES-256-GCM, zeroize, SQLx | CRITICAL: Angel credentials |
| `angel.rs` | session, quotes/candles, place/cancel, order book, positions, RMS/margin; error classification | reqwest, egress | CRITICAL: only live broker REST mutation layer |
| `egress.rs` | inventory APIs, assignment, verification, rehydration, `source_ip_for_user`, bound HTTP client | SQLx, Unix socket, reqwest | CRITICAL: broker IP/account correctness |
| `market_ws.rs` | `/api/ws/market`, Angel WS connect/bind/auth/subscribe, shared feeds | Axum WS, tokio-tungstenite | HIGH: price truth and source isolation |
| `strategy.rs` | `futures_breakout_v3`, `supertrend_index_options_v1`, signals/intents, orders/fills, SL/target, recovery/reconciliation, workers | nearly all backend modules | CRITICAL |
| `risk.rs` | global/user limits, kill switches, atomic decision/reservation | SQLx transactions/advisory locks | CRITICAL |
| `backtesting.rs` | futures backtest, stored runs/trades, XLSX export | SQLx, Angel candles, rust_xlsxwriter | MEDIUM; broker reads only |
| `contract_master.rs` | Angel instrument-master cache and contract resolution | HTTP, serde, Tokio cache | HIGH: token, lot and tick selection |
| `instruments.rs` | supported futures metadata and quantity/P&L unit conversion | static domain rules | HIGH: financial quantity correctness |
| `account.rs` | profile OTP change, trading-mode switch | auth, credentials, SQLx | HIGH: demo/live boundary |
| `home.rs` | status, Angel login/connect, API-key profile update | credentials, Angel | HIGH: broker session lifecycle |
| `pnl.rs` | paginated P&L and export | SQLx, XLSX | MEDIUM |
| `jobs.rs` | OTP cleanup, session audit, strategy reload | SQLx, strategy | MEDIUM/HIGH depending on reload |
| `logs.rs` | file listing and bounded content read | filesystem, auth | MEDIUM: information exposure |
| `audit.rs` | audit records and webhook/email alerts | SQLx, reqwest, lettre | HIGH: evidence/operations |
| `alerts.rs` | durable operational alert construction/delivery coordination | audit, notification channels | HIGH: safety visibility |
| `notifications.rs` | user notification and broadcast helpers | strategy events, SQLx | MEDIUM |
| `ops.rs` | liveness, readiness and metrics handlers | PostgreSQL, process state | HIGH: deployment gating |
| `state.rs` | shared `AppState`, caches, locks, channels and semaphores | all runtime clients/state | CRITICAL: ownership/concurrency |
| `error.rs` | `AppError`, response conversion | Axum | HIGH: API compatibility and secret masking |
| `models.rs` | users/profiles/credentials and shared records | serde, SQLx | HIGH |
| `bin/rulenix-egress-helper.rs` | host address configuration, namespace alias, route, SNAT, verification | root OS tools, Unix socket | CRITICAL: privileged networking boundary |

Rust dependencies that need explicit Python equivalents include Axum, Tokio, SQLx, reqwest, tokio-tungstenite, serde, chrono, uuid, Argon2, AES-GCM, HMAC/SHA-256, Lettre, tracing, and rust_xlsxwriter.

## 2. Runtime architecture

```text
Browser ─TLS─> Caddy/Nginx ─> React static files
                         └──> Rust Axum HTTP/WS ─> PostgreSQL 16
                                               ├─> Angel REST
                                               ├─> Angel SmartAPI WebSocket V2
                                               ├─> SMTP / alert webhook
                                               └─Unix 0600 socket─> root egress helper
                                                                      ├─netplan host /32
                                                                      ├─container netns alias
                                                                      ├─return route
                                                                      └─per-IP SNAT
```

Startup order in `main.rs` is material behavior:

1. load dotenv and environment; configure JSON tracing for staging/production;
2. validate `Config`; open PostgreSQL pool (10 connections; production TLS `verify-full`);
3. acquire session advisory lock `hashtext('rulenix:migrations')`, run embedded SQLx migrations, unlock;
4. honor explicit CLI-only migrate/credential-rotation/admin bootstrap modes;
5. initialize credential store and migrate legacy plaintext credentials under a lock;
6. apply `FORCE_DEMO_TRADING` only where no open/nonterminal live exposure makes conversion unsafe;
7. build global 15-second HTTP client and in-memory channels/locks/caches/semaphores;
8. in production, rehydrate every configured egress address before workers start;
9. emit service-start alert; start strategy scheduler, broker-session maintenance, and session cleanup;
10. bind Axum with request-ID, sensitive-header handling, body limit, CORS, auth/CSRF middleware and routes.

Shutdown handles Ctrl-C and Unix SIGTERM and gracefully drains HTTP. Tokio tasks are not individually checkpointed/drained; process exit terminates them. Correctness therefore depends on durable database states and startup recovery, not in-memory completion.

### Long-running tasks

| Task | Cadence/trigger | Work | Coexistence rule |
|---|---|---|---|
| strategy scheduler leader | loop about every 5 s; DB advisory leader lock | schedules strategies, claims intents, recovery, broker reconciliation, protection, reversal, square-off | **MUST NOT** be active in Rust and Python |
| execution-intent workers | spawned from due intents; semaphore 8 | risk, reservation, order submission | **MUST NOT** overlap |
| reconciliation | about every 5 s within scheduler | Angel order book/positions, fill/status/incident repair | one mutating authority only |
| protection recovery | about every 5 s | create/recover SL and target; emergency containment | one authority only |
| reversal recovery | scheduler loop | durable SL2 reversal intent | one authority only |
| shared market feeds | reconnecting per exchange/token demand | Angel tick ingestion, tick persistence, demo fills | only one demo-fill/mutating feed authority |
| browser market bridge | one per browser WS | direct authenticated Angel market stream | may coexist only if isolated and read-only |
| broker session maintenance | every 60 s | validate/refresh expiring Angel sessions | one credential writer only |
| expired session cleanup | every 3600 s | delete/revoke expired app sessions | coordinate DB ownership |
| egress startup rehydration | once before worker activation | privileged configure/verify | never run concurrently from two backends |
| manual admin jobs | HTTP-triggered spawn | OTP cleanup, session audit, strategy reload | strategy reload needs single owner |

Python shadow mode may run HTTP health/read-only comparison and independently consume replicated/read-only data. It must not claim intents, schedule, reconcile mutably, simulate fills, refresh credentials, cancel, protect, configure egress, or run operational jobs.

## 3. Complete API contract

> Historical `09a03a1...` inventory. Apply the additions and changed schemas in section 36.2; the authoritative current total is 50 HTTP contracts plus two WebSockets across 46 paths.

There are **48 ordinary HTTP method/route contracts and two authenticated browser WebSocket upgrades**, across **45 unique paths**. Unless stated public, requests use the opaque session cookie. POST/PUT/PATCH/DELETE protected routes require `X-CSRF-Token` matching the readable CSRF cookie. JSON request structs use `deny_unknown_fields`. Standard application errors are `{"detail": string, "retry_after": number|null}` with 400/401/403/404/429/500 as applicable; 429 also sets `Retry-After`. Internal error detail is masked. Framework-level malformed JSON/body-limit errors must also remain frontend-compatible.

Abbreviations: `S` session, `C` CSRF, `A` administrator, `P` permission-specific, `DB` database effect, `BR` broker read, `BM` broker mutation.

| Method and route | Rust handler (`backend/src/...`) | Request / response contract | Access | Effects and known consumer |
|---|---|---|---|---|
| GET `/api/health` | `ops.rs::liveness` | liveness JSON | public | no readiness dependency; compatibility health URL |
| GET `/api/health/live` | `ops.rs::liveness` | liveness JSON | public | none; orchestrator |
| GET `/api/health/ready` | `ops.rs::readiness` | readiness JSON | public | DB/dependency readiness; proxy/load tests |
| GET `/api/metrics` | `ops.rs::metrics` | process/app metrics JSON | S | DB reads; documented as internal/admin but code permits any authenticated user |
| POST `/api/auth/request-otp/` | `auth.rs::request_otp` | `{email,username}` → generic result | public/rate-limited | OTP DB write, SMTP |
| POST `/api/auth/signup/` | `auth.rs::signup` | `{username,user_id,api_key,mobile,email,password,confirm_password,otp}` | public/rate-limited | user/profile/encrypted secret/OTP/audit DB writes |
| POST `/api/auth/login/` | `auth.rs::login` | `{username,password}` → user + cookies | public/rate-limited | lockout/session DB writes; frontend auth |
| POST `/api/auth/password/request-reset/` | `auth.rs::request_reset` | `{email}` → generic result | public/rate-limited | OTP/SMTP |
| POST `/api/auth/password/verify-otp/` | `auth.rs::verify_reset` | `{email,otp}` | public/rate-limited | OTP attempt/read |
| POST `/api/auth/password/reset/` | `auth.rs::reset_password` | `{email,otp,password,confirm_password}` | public/rate-limited | password change; revoke sessions |
| GET `/api/auth/access/` | `auth.rs::access_status` | current user/permissions/mode | S | DB read; frontend route guards |
| POST `/api/auth/logout/` | `auth.rs::logout` | result and cleared cookies | S+C | revoke session |
| GET `/api/auth/admin/users/` | `auth.rs::list_users` | users/profiles list | S+A | DB read; Admin Users page |
| PATCH `/api/auth/admin/users/` | `auth.rs::update_user` | `{username, can_administer?, can_live_trade?, can_backtest?, can_backtest_on_trading_days?}` | S+C+A | permission DB/audit write |
| DELETE `/api/auth/admin/users/` | `auth.rs::delete_user` | JSON `{username}` (other permission fields optional but unused) → 204 | S+C+A | guarded user deletion; refuses open exposure |
| DELETE `/api/auth/admin/users/trade-logs/` | `auth.rs::clear_user_trade_logs` | JSON `{username}` | S+C+A | requires global kill; Angel order/position BR before closed-live deletion; demo reset can proceed fail-closed; no broker mutation |
| GET `/api/auth/admin/trades/daily/` | `auth.rs::daily_trade_report` | optional `date` → daily list | S+A | DB read |
| GET `/api/admin/egress-ips` | `egress.rs::list` | inventory + assignment view | S+A | DB read; Admin Egress page |
| POST `/api/admin/egress-ips` | `egress.rs::add` | `{ip_address}` → egress item | S+C+A | DB write + privileged configure/verify; no broker order |
| POST `/api/admin/egress-ips/{id}/verify` | `egress.rs::verify` | path UUID → item | S+C+A | helper configure/verify + DB/audit |
| PUT `/api/admin/users/{user_id}/angel-egress` | `egress.rs::assign` | `{egress_ip_id: UUID|null}` | S+C+A | DB assignment; configured target required |
| GET `/api/home/status/` | `home.rs::status` | dashboard/user/broker status | S | DB read |
| POST `/api/home/connect/` | `home.rs::connect` | `{mpin,totp}` → connection state | S+C | Angel login/session BR; encrypted token DB write |
| PATCH `/api/home/profile/` | `home.rs::update_profile` | `{api_key}` | S+C | encrypted credential/profile update |
| GET `/api/account/profile` | `account.rs::get_profile` | account profile/permissions/mode | S | DB read; Account page |
| POST `/api/account/profile/request-otp` | `account.rs::request_profile_otp` | `{}` | S+C | OTP DB/SMTP |
| PATCH `/api/account/profile` | `account.rs::update_profile` | `{otp,new_username,email,mobile_number,client_id}` | S+C | locks user, DB/audit/session effects |
| PUT `/api/account/trading-mode` | `account.rs::update_trading_mode` | `{mode,confirm_live?}` | S+C | guarded demo/live DB change; Angel session BR when live |
| GET `/api/pnl` | `pnl.rs::list` | `page?,page_size?,mode?` → rows + pagination/totals | S | own-trade DB read; P&L page |
| GET `/api/pnl/export` | `pnl.rs::export` | same filter → XLSX | S | DB read/file response |
| GET `/api/backtesting/runs` | `backtesting.rs::history` | run list | S+P(backtest) | DB read |
| GET `/api/backtesting/runs/{run_id}/export` | `backtesting.rs::export` | UUID → XLSX | S+P | own/admin DB read |
| POST `/api/backtesting/run` | `backtesting.rs::run` | `{strategy_key?,instrument?,interval?,lookback_months,lots}` | S+C+P | Angel candle BR; persisted run/trades; no live write |
| GET `/api/logs/files/` | `logs.rs::files` | file metadata list | S | own logs; admin visibility broader |
| GET `/api/logs/content/` | `logs.rs::content` | `filename,lines?,tail?,since_session?` → bounded text | S | filesystem read, max 2 MiB; path constrained |
| GET `/api/scheduler/jobs/` | `jobs.rs::list` | job metadata/history | S+A | DB read |
| POST `/api/scheduler/trigger/` | `jobs.rs::trigger` | `{job_key}` | S+C+A | spawns approved maintenance job |
| GET `/api/risk/admin` | `risk.rs::admin_status` | global/user limits and switches | S+A | DB read |
| PUT `/api/risk/admin/limits` | `risk.rs::update_global_limits` | optional limit fields | S+C+A | locked global risk DB write/audit |
| PUT `/api/risk/admin/limits/{user_id}` | `risk.rs::update_user_limits` | optional limit fields | S+C+A | locked user risk DB write/audit |
| PUT `/api/risk/admin/kill-switch` | `risk.rs::update_global_kill` | `{enabled,reason?}` | S+C+A | persistent switch; cancels nonterminal entries if enabling (**BM cancel possible**) |
| PUT `/api/risk/admin/kill-switch/{user_id}` | `risk.rs::update_user_kill` | `{enabled,reason?}` | S+C+A | same for user; **BM cancel possible** |
| GET `/api/strategy/futures-breakout` | `strategy.rs::status` | `instrument?` → config/snapshot/orders/trades/alerts | S | DB read; strategy page |
| PUT `/api/strategy/futures-breakout` | `strategy.rs::update` | `{strategy_key?,instrument?,enabled,lots,run_day_session?,run_evening_session?,target_points?,stop_loss_points?}` | S+C | guarded configuration write |
| GET `/api/strategies` | `strategy.rs::catalog` | catalog + activation/config state | S | DB read |
| GET `/api/strategies/admin/executions` | `strategy.rs::admin_execution_report` | `date?` → signals/intents | S+A | DB read |
| POST `/api/strategies/admin/executions/retry` | `strategy.rs::admin_retry_execution_intent` | `{intent_id}` | S+C+A | resets eligible durable intent; later worker can **BM place** |
| PUT `/api/strategies/{strategy_key}/activation` | `strategy.rs::update_activation` | `{active}` | S+C | DB write; deactivation may cancel active entry orders (**BM cancel possible**) |
| WS `/api/ws/market?tokens=...&exchange_type?&mode?` | `market_ws.rs::upgrade` | binary/text Angel tick bridge, errors/heartbeat | S | Angel WS BR, no order mutation |
| WS `/api/ws/strategy` | `strategy.rs::events_upgrade` | server JSON event broadcasts; client Ping/Close | S | scoped global/user events, no broker effect |

Frontend API calls are centralized under `frontend/src`; current pages consume auth/access, account, home, P&L, backtesting, logs, strategy, risk/admin, user admin, scheduler, and egress endpoints. No `new WebSocket` consumer was found in the current frontend source, so both WS APIs are public backend contracts but appear unused by this React revision.

## 4. WebSocket contracts

### Browser WebSockets

`/api/ws/market` authenticates the normal session before upgrade, requires a nonempty comma-separated token list, loads that user's Angel credentials, and refuses an unconnected session. It opens a user-specific Angel stream, subscribes using `exchange_type` and `mode` (Angel WS V2 supports grouped exchanges), forwards market messages, emits JSON error detail on failure, and records session logs. Browser ping/pong and stale detection must remain compatible. The bridge heartbeat is about 10 seconds and browser staleness about 30 seconds.

`/api/ws/strategy` authenticates before upgrade and subscribes to a Tokio broadcast channel of capacity 1,024. JSON values with no `user_id` are broadcast to all connected users; values with `user_id` are delivered only to that UUID. Client `Ping` receives `Pong`; `Close`/EOF ends the loop. Lagged channel messages are skipped rather than replayed; reconnect has no server-side replay contract, so clients must refresh REST state.

### Angel WebSocket

Angel SmartAPI WebSocket V2 authentication uses brokerage client ID, JWT and feed token from the encrypted per-user store. DNS is resolved to IPv4, a `TcpSocket` is created, and an explicitly assigned user is bound to the deterministic private alias corresponding to the verified public egress address before TCP/TLS. Bind/connect failures have **no default-route fallback**. NULL assignment creates an unbound socket and follows normal OS routing.

The direct browser bridge uses the requesting account. Shared strategy feeds select one recently connected/eligible Angel account to source market data for the requested exchange/token set. That means shared ticks are not order credentials, but the selected feed account and its egress are shared infrastructure; Python must make this selection explicit, observable, and tested for account/IP isolation. Feeds heartbeat about every 10 seconds, regard data as stale after about 45 seconds for equities and 120 seconds for MCX, refresh subscriptions about every 5 seconds, and reconnect with exponential backoff roughly 1–90 seconds (rate-limit delay around 60 seconds plus jitter). Required resubscription state is rebuilt after reconnect.

## 5. PostgreSQL contract

> Historical schema narrative. Section 36.3 is authoritative for the 49-migration, 37-table, one-view production catalog and adds the manual-close/readiness objects omitted here.

The backend uses SQLx async PostgreSQL with embedded migrations and extensive raw SQL. The first Python version must use the existing schema exactly; Alembic may record future migrations but must not bootstrap or redesign these tables.

### Table inventory (32 tables)

| Table | Purpose / important identity and constraints |
|---|---|
| `users` | UUID user; case-insensitive unique username/email; password hash, permissions, lockout/password-change timestamps |
| `user_profiles` | user PK/FK; brokerage client/mobile, mode, demo balance, token state/times, credential revision, unique nullable egress FK |
| `email_otps` | UUID; email/purpose (`signup/reset/profile`), HMAC, expiry, used/attempt/cooldown/invalidation state |
| `trades` | UUID user trade; demo/live, open/closed, direction, quantity, numeric prices/P&L, strategy/snapshot/order metadata, protection/safety state, reversal and exit audit |
| `job_runs` | administrative job history/status |
| `strategy_market_snapshots` | UUID; unique strategy/instrument/date execution key; contract/levels/gap readiness snapshot |
| `user_strategy_configs` | composite user/strategy/instrument configuration |
| `strategy_orders` | UUID; trade/snapshot/user, role/side/type/status, broker/client/idempotency keys, quantities/fill watermarks/prices/error/version |
| `strategy_events` | bigserial append-like strategy and operational events |
| `user_strategy_activations` | composite user/strategy activation |
| `strategy_scheduler_runs` | UUID; unique strategy/instrument/date/session/action dispatch record |
| `market_calendar` | date PK and trading-day/session metadata |
| `user_sessions` | UUID; unique SHA-256 token hash, CSRF hash, idle/absolute expiry, revoke/password fence; partial active indexes |
| `broker_secrets` | composite user/secret-kind; AES-GCM ciphertext/nonce/key version; allowed kinds API/JWT/refresh/feed |
| `risk_limits` | nullable global or per-user limit row; unique partial indexes and numeric limits |
| `risk_kill_switches` | nullable global or per-user persistent switch; unique partial indexes |
| `market_price_ticks` | latest tick keyed by exchange/token with price/time |
| `broker_reconciliation_health` | per-user last success/error and health state |
| `risk_decisions` | UUID allow/deny evidence, exposure inputs and reason |
| `broker_order_events` | bigserial normalized broker status/fill observations linked to order/user |
| `audit_events` | immutable security/administration evidence; DB triggers reject update/delete |
| `alert_delivery_attempts` | operational alert channel attempts/result |
| `backtest_market_candles` | cached historical candles |
| `backtest_runs` | UUID run/config/result metadata |
| `backtest_trades` | simulated trades linked to run |
| `broker_margin_estimates` | legacy cached margin estimates retained by schema |
| `strategy_reversal_intents` | durable SL2 reversal state and retry metadata |
| `backtest_option_contracts` | legacy option contract snapshots retained by schema |
| `strategy_signals` | durable signal; unique strategy/instrument/session/type and fan-out counts/status |
| `strategy_execution_intents` | per-user durable entry/squareoff intents; attempts, claim/due/status/error; partial uniqueness prevents duplicate fan-out |
| `broker_position_incidents` | unresolved/resolved broker/local position mismatch; unique user/exchange/token/type |
| `broker_egress_ips` | UUID inventory; unique public IPv4 `/32` INET; configuration/verification/availability timestamps and creator |

Domain enums are strings constrained with SQL `CHECK`, not PostgreSQL enum types. Python must preserve exact spelling and reject unrecognized values instead of coercing them.

### Concurrency and idempotency mechanisms

- session advisory lock serializes migrations; another advisory leader lock permits one scheduler;
- global shared and per-user exclusive advisory locks make risk check plus exposure reservation atomic with admin changes;
- per-user advisory locks serialize account/profile/session cleanup operations; egress operations also lock user and egress identities;
- OTP, session, order-fill/trade and other state transitions use transactions and `SELECT ... FOR UPDATE`;
- intent claiming uses `FOR UPDATE SKIP LOCKED` and a durable `claimed` state;
- unique execution keys, broker client tags, idempotency keys, signal/audience keys and partial indexes prevent duplicate materialization;
- order version/status and cumulative fill watermarks prevent regression/double accounting;
- database triggers enforce valid order transitions, exit coverage/terminal safety, and immutable audits.

Python must reproduce the guarantee, not the Rust primitive: multiple processes must remain safe, so process-local `asyncio.Lock` is never an adequate substitute for DB locks/constraints.

## 6. Authentication, users, and administration

Application authentication is not JWT-based. It uses opaque server-side sessions:

- random 32-byte URL-safe session and CSRF tokens; only SHA-256 hashes stored in DB;
- `rulenix_session` is HttpOnly and `rulenix_csrf` is readable; both SameSite=Lax, Path `/`, Secure in HTTPS/production;
- authentication joins active user/profile, enforces idle and absolute expiry and a `password_changed_at` fence;
- mutation methods validate `X-CSRF-Token`; successful activity extends idle expiry without crossing absolute expiry;
- logout/revocation and hourly cleanup persist in PostgreSQL.

Passwords are Argon2 hashes with random salt. Policy is 12–128 characters with upper/lower/digit/symbol, no whitespace, and no username/email-local-part inclusion. Login has process-local request gates plus database exponential lockout. OTPs are six digits, HMAC-SHA-256 under `OTP_HASH_KEY`, purpose-bound, attempt-limited, expiry/cooldown protected, consumed under row lock, and use enumeration-resistant public responses.

Permissions are independent booleans: `can_administer`, `can_live_trade`, `can_backtest`, and `can_backtest_on_trading_days`. `trading_mode` is independently `demo` or `live`. Selecting live requires live permission, explicit confirmation, a valid broker session, and no unsafe active execution. Admin handlers explicitly recheck administration permission; session authentication alone is not authorization.

Broker secrets use AES-256-GCM with a random 12-byte nonce, configured key version, and AAD `rulenix:broker-secret:{user}:{kind}:v{version}`. Key kinds are API key, JWT, refresh token, and feed token. Debug output is redacted and decrypted values are zeroized where supported. Startup migrates legacy plaintext and database checks require those fields to remain blank. MPIN/TOTP are used for Angel login but not persisted. Token writes use the profile credential revision as compare-and-swap protection against stale concurrent responses.

## 7. Configuration inventory

Never put values for secret-bearing variables into migration documentation or tests.

| Category | Names and semantics |
|---|---|
| application | `APP_ENV`, `HOST`, `PORT`, `FRONTEND_ORIGINS`, `MAX_REQUEST_BODY_BYTES`, `TRUSTED_PROXIES` |
| database | `DATABASE_URL`; `TEST_DATABASE_URL` only for explicitly disposable loopback DB named `rulenix_test_*`; Compose `POSTGRES_DB`, `POSTGRES_USER`, `POSTGRES_PASSWORD_FILE` |
| application auth | `OTP_HASH_KEY`, OTP TTL/attempt/resend variables; request-rate and login-lockout variables; `SESSION_IDLE_MINUTES`, `SESSION_ABSOLUTE_HOURS` |
| credential encryption | `CREDENTIAL_ENCRYPTION_PRIMARY_VERSION`, `CREDENTIAL_ENCRYPTION_KEYS` |
| email/alerts | `SMTP_HOST`, `SMTP_PORT`, `SMTP_USERNAME`, `SMTP_PASSWORD`, `SMTP_FROM`, `ALERT_WEBHOOK_URL`, `ALERT_EMAIL_TO` |
| Angel | `ANGEL_API_BASE`, `ANGEL_WS_URL`, `CLIENT_PUBLIC_IP`, `CLIENT_LOCAL_IP`, `CLIENT_MAC_ADDRESS` |
| trading/safety | `FORCE_DEMO_TRADING`, `PROTECTION_ACK_TIMEOUT_SECONDS`, `PROTECTION_MAX_ATTEMPTS`, `AMBIGUOUS_ORDER_TIMEOUT_SECONDS`, `MARGIN_SAFETY_BUFFER_PERCENT` |
| egress backend | `EGRESS_HELPER_SOCKET` |
| privileged helper | `RULENIX_EGRESS_SOCKET`, `RULENIX_EGRESS_INTERFACE`, `RULENIX_EGRESS_NETPLAN_FILE`, `RULENIX_EGRESS_STATE_FILE`, `RULENIX_EGRESS_SOCKET_UID`, `RULENIX_EGRESS_VERIFY_URL` |
| logging | `RUST_LOG`, `RULENIX_LOG_DIR` |
| bootstrap | `INITIAL_ADMIN_USERNAME`, `INITIAL_ADMIN_EMAIL`, `INITIAL_ADMIN_PASSWORD` (CLI/bootstrap only) |
| frontend/dev/load | `RULENIX_FRONTEND_PORT`, `RULENIX_BACKEND_URL`, `RULENIX_BASE_URL` |

Important defaults include port 8080, 64 KiB request bodies, OTP 10 minutes/5 attempts/60-second resend, sessions 30-minute idle/24-hour absolute, global HTTP timeout 15 seconds, protection acknowledgement 30 seconds/3 attempts, ambiguous order timeout 120 seconds, and margin buffer 10%. Production validation rejects insecure database/origin settings, placeholder crypto secrets, missing SMTP, loopback public IP, and zero MAC. Python must fail startup on the same invalid production conditions.

## 8. Angel REST contract

> Historical REST inventory. Current production also contains backend trade-book and conditional/GTT reads plus operational-gate individual-order detail reads; see sections 36.4 and 36.6. Place and Cancel remain the only trading mutation primitives.

The request chain is:

```text
authenticated Rulenix user
  → user_profiles brokerage identity
  → encrypted broker_secrets and session state
  → egress.http_client_for_user(user_id)
  → unbound default client OR explicitly source-bound client
  → Angel headers/authentication
  → SmartAPI REST
```

| Classification | Rust operation in `angel.rs` | Angel path | Timeout/retry/outcome behavior |
|---|---|---|---|
| BROKER_READ_ONLY/session | create session | `/rest/auth/angelbroking/user/v1/loginByPassword` POST | 15 s client; response parsed and tokens encrypted; login-specific bounded handling |
| BROKER_READ_ONLY | `market_quote` | `/rest/secure/angelbroking/market/v1/quote` POST | gated; dynamic JSON validated; errors classified |
| BROKER_READ_ONLY | `get_candles` | `/rest/secure/angelbroking/historical/v1/getCandleData` POST | chunked requests with about 400 ms spacing |
| **BROKER_LIVE_MUTATION** | `place_order` | `/rest/secure/angelbroking/order/v1/placeOrder` POST | rate-gated; never blind-retry ambiguous delivery; stable client tag |
| **BROKER_LIVE_MUTATION** | `cancel_order` | `/rest/secure/angelbroking/order/v1/cancelOrder` POST | gated; callers reconcile terminal outcome |
| BROKER_READ_ONLY | `order_book` | `/rest/secure/angelbroking/order/v1/getOrderBook` GET | roughly 1/s gate; reconciliation source |
| BROKER_READ_ONLY | `positions` | `/rest/secure/angelbroking/order/v1/getPosition` GET | reconciliation source |
| BROKER_READ_ONLY | `rms_limits` | `/rest/secure/angelbroking/user/v1/getRMS` GET | live capital precheck |
| BROKER_READ_ONLY | `margin_required` | `/rest/secure/angelbroking/margin/v1/batch` POST | calculation/read despite POST method |
| BROKER_READ_ONLY/session | `refresh_session` | `/rest/auth/angelbroking/jwt/v1/generateTokens` POST | refreshes encrypted tokens with revision guard |

Not implemented in repository backend: Modify Order, trade book, holdings, individual-order lookup, conditional/GTT inventory, GTT/OCO create/modify/cancel. These absences are contract facts. No Python code may invent their behavior; the missing OCO audit artifact must first define the read APIs it actually used.

Angel request gates are process-local and keyed by API identity/path. Place/cancel aggregate limits are documented near 8/second, 450/minute and 900/hour, with cooldown commonly 90 seconds and capped around 300 seconds while honoring `Retry-After`. Python must verify exact constants from tests/source during implementation.

A write outcome distinguishes failure before the request can reach Angel (`Retryable`) from a timeout/lost response after possible delivery (`Ambiguous`). Ambiguous place outcomes are persisted and reconciled by broker order ID or stable `RX...` client tag; they are not resubmitted merely because no response was received. Rejections/auth failures are separate terminal/actionable classes.

## 9. Per-user static egress IP

### Application semantics

```text
user_profiles.broker_egress_ip_id IS NULL
  → source_ip_for_user returns None
  → shared reqwest client / ordinary TCP socket
  → no application source bind
  → host/OS default routing (currently REPORTED as 139.99.155.62)

explicit inventory address X
  → require CONFIGURED and VERIFIED
  → derive stable private CGNAT binding alias A(X)
  → bind REST client or Angel WS TCP socket to A(X)
  → host SNAT maps A(X) to public X
  → any selection/bind/connect/configuration failure blocks the broker operation
```

NULL does not mean “bind `139.99.155.62`.” It means do not bind. The actual default public address is an operational routing property.

Admin add validates a public IPv4, creates inventory, and immediately asks the helper to configure/verify it. Verify forces another helper pass. Assignment first proves the row is configured and verified, then serializes user/address updates with advisory locks and the unique profile FK. Unassignment stores NULL; it does not remove a host IP or its inventory row. Startup rehydrates all `CONFIGURED` rows before workers. Rehydration failure updates readiness state and makes explicit users fail closed.

### Bridged-container networking implementation

Production release `09a03a1…` does **not** bind a public /32 directly inside the bridged backend container. `egress::binding_ipv4(public)` deterministically maps each public address into a distinct `100.64.0.0/10` private alias. The root helper then:

1. validates that the requested address is public IPv4;
2. persists/adds the public `/32` to the host interface (reported interface `ens3`) using a managed netplan file, backing up and rolling back if `netplan generate` fails;
3. locates the backend container from Docker labels and obtains its PID;
4. uses `nsenter` into that network namespace and adds the private alias `/32` to container loopback;
5. discovers the container `eth0` address and host bridge and adds a host return route for the alias via the container;
6. installs idempotent, commented iptables `POSTROUTING` SNAT from alias out the host interface to the selected public IP;
7. verifies from inside the container namespace by binding curl to the alias and reaching a configured HTTPS public-IP service with pinned resolution; observed public IP must exactly equal X.

The release added `CAP_SYS_PTRACE` to the helper service so namespace entry works, alongside narrowly declared capabilities required for networking/files. The Unix socket defaults to `/run/rulenix-egress/helper.sock`, mode 0600, owned for backend UID 10001. The request protocol exposes only configure-and-verify, and helper operations are serialized. The helper is a separate privileged security boundary and should initially remain a reviewed standalone component; a Python rewrite is not a prerequisite.

REST uses `reqwest::ClientBuilder::local_address(alias).no_proxy()` for explicit accounts. It builds an explicit client per operation rather than sharing an IP-specific pool; NULL users share the global pool. Angel WS uses a bound `TcpSocket`. This prevents an explicit account from silently borrowing the default route, but it sacrifices explicit-client connection pooling. No credentials are stored in the HTTP client itself.

Known egress race: changing a DB assignment does not forcibly tear down an already-open Angel WebSocket; the new address takes effect on a later connection. Assignment also does not appear to reject a user merely because trading is active. This is HIGH technical debt: until corrected in a separate safety phase, operations must prohibit assignment changes during exposure and record/reconnect affected sessions deterministically.

The checked-out dirty worktree contains older untracked egress/helper copies that bind differently. They are not authoritative. Migration work must start from the production Git object, not those files.

## 10. Trading domain and state machines

Core entities are durable strategy snapshot → signal → user execution intent → strategy order → fill deltas → trade and exits. Market ticks, risk decisions, broker order events, reconciliation health/incidents, reversal intents and scheduler runs preserve supporting truth.

### Order states

```text
pending ──> submitting ──> submitted ──> partially_filled ──> filled
   │             │             │  └──> processing ─────────────┘
   │             │             │  └──> cancelling ──> cancelled
   │             └──> ambiguous ────────┘       └──> filled/rejected
   └──> failed ──> pending (explicit retry)
   └──> rejected / cancelled
```

Permitted transitions are encoded in SQL trigger logic and mirrored in Rust. Pending can become submitting/submitted/failed/rejected/cancelled. Submitting can become submitted/ambiguous/failed/rejected/cancelled. Ambiguous, submitted, partially-filled and processing can advance to broker-observed processing/fill/reject/cancel states and, where allowed, cancelling. Cancelling can return to a broker-observed active/partial state or become filled/rejected/cancelled. `filled`, `rejected`, and `cancelled` are terminal. `failed` can only return to `pending` through an explicit controlled retry. Python must use the database transition function as final authority and add golden transition tests.

### Trade and protection states

The business trade status is only `open` or `closed`. A parallel safety lifecycle carries the meaningful protection state:

```text
DEMO
PROTECTION_REQUIRED → PROTECTION_SUBMITTING → PROTECTION_UNCERTAIN
                    └──────────────────────→ PROTECTED
                    └──────────────────────→ PROTECTION_FAILED
PROTECTED/FAILED/UNCERTAIN → CLOSING → CLOSED
                         └→ EMERGENCY_CLOSING → RECONCILIATION_REQUIRED/CLOSED
```

Database constraints forbid closed-state regression and regression from emergency containment to an earlier unsafe state. Exact Rust transition guards and SQL checks both need parity tests.

Other durable state machines:

- signal: `confirmed → dispatching → completed | partial | failed | expired`;
- execution intent: `pending → claimed → retry_wait | submitted | completed | skipped | failed | expired`, with stale `claimed` recovered;
- scheduler run: `pending → running → completed | failed | skipped`, with stale running recovered to failed;
- reversal intent: `pending → processing → waiting | submitted → completed | failed | cancelled`;
- egress configuration: configuration pending/configured/failed plus independent verified/unverified/verification-failed/availability state.

Invalid state, unknown broker status, quantity regression, or missing evidence must produce an incident/error and stop unsafe progression; it must not be coerced to a convenient terminal state.

## 11. Complete order lifecycle

The actual execution call chain is concentrated in `strategy.rs`:

```text
scheduler / completed market candle
→ materialize durable strategy_signal and audience intents in one transaction
→ claim due strategy_execution_intents FOR UPDATE SKIP LOCKED
→ semaphore-bounded worker (8)
→ revalidate user activation, permissions, mode, account, session and signal freshness
→ place_strategy_order / place_strategy_order_inner
→ resolve current contract lot size and tick size; normalize side/quantity/prices
→ quote/circuit and duplicate-exposure checks
→ risk::assess_and_reserve under global/user locks
→ create durable strategy_orders row with idempotency key and stable Angel client tag
→ final live gate under locks (FORCE_DEMO, kill switch, permissions, credentials)
→ demo simulation OR angel::place_order [LIVE_BROKER_MUTATION]
→ submitted / rejected / retryable failure / ambiguous
→ reconciliation observes cumulative fills and applies monotonic fill delta
→ entry-fill transaction serializes user/instrument and creates/updates trade
→ submit exact stop slice and wait for broker acknowledgement
→ submit target only after protective acknowledgement
→ target/SL partial fills update trade and sibling quantities/cancellation
→ close or emergency/double-fill containment
→ reconciliation confirms terminal broker/local position truth
```

Both BUY and SELL entry directions are supported. Orders include MARKET, LIMIT, STOPLOSS_LIMIT and STOPLOSS_MARKET paths as required by the two strategies. Quantity is integer shares/contracts derived from lots × current contract lot size, never accepted blindly from UI.

Partial fills use broker cumulative fill quantity as a watermark; only a positive new delta is accounted. Entry fill processing serializes by user/instrument to resolve same-side aggregation versus opposite-side close/reversal deterministically. Stop slices are created for the exact filled delta before an unprotected target. Partial target fills book realized P&L and reduce remaining exposure/stop coverage. A sibling cancel request is not assumed terminal until broker observation.

If both exits fill and create broker over-close, the backend reconstructs the residual direction as a `broker_over_close` trade and sends an exact emergency close rather than hiding the mismatch. This path is CRITICAL parity behavior.

### Ambiguous broker write

If request transmission may have reached Angel but Rulenix loses the response, the order becomes `ambiguous`. It is **not** placed again. Reconciliation searches the order book by broker ID or stable client tag. If absent until `AMBIGUOUS_ORDER_TIMEOUT_SECONDS`, it raises an operator/reconciliation incident rather than guessing “not placed.” This is the central duplicate-order protection.

## 12. Stop loss, target, and OCO

> The historical missing-source finding below was resolved by the source-controlled classifier/gate in the current release. Section 36.4 is authoritative; the strict conjunction remains unchanged.

Protection is application-managed with ordinary Angel orders; repository source contains no GTT/OCO mutation API. For each confirmed entry-fill slice, Rulenix persists protection-required state, submits the stop first, waits for broker acknowledgement, and then creates target coverage. Multi-lot futures split target quantity: one lot exits fully at TP1; for more lots TP1 takes `ceil(lots/2)` and the runner remains under SL2/reversal handling. Supertrend options use a STOPLOSS_MARKET stop and limit target with instrument-specific point defaults.

When a target partially/fully fills, Rulenix books only the fill delta, recalculates remaining exposure and cancels/adjusts relevant sibling protection through cancel-and-recreate/recovery logic. There is no Angel Modify Order call. SL fill, target fill, cancel/fill race, child rejection, ambiguous child submission and manual broker actions all route through reconciliation/protection recovery. No local cancellation result alone proves the sibling cannot fill.

### Required synthetic OCO classifier contract (operational evidence; source missing)

The known informational row may be classified non-executable **only** if every predicate is proven:

```text
strict known Android synthetic record shape
AND matching broker position is flat
AND individual-order lookup for the exact unique ID returns AB1007 Order not found
AND conditional/GTT/OCO inventory read succeeded and contains no matching rule
AND trade-book read succeeded and contains no exact trade
AND there is no executable sibling order
```

The reported strict shape for `NIFTY01SEP2624100CE` was: Angel type `OCO_LIMIT`, matching Android strategy marker, `SE-` unique-ID prefix, no broker/exchange/parent ID, no status, no prices, and no executable, pending, or fill quantity; it appeared one second after a manual exit. Shape alone, symbol alone, `SE-` alone, and especially `OCO_LIMIT` alone are insufficient.

Any failed broker read, malformed/missing field, non-flat/unknown position, active/trigger-pending/filled/cancelled/rejected/expired order evidence, matching conditional rule, exact trade, or executable sibling yields **UNKNOWN/UNSAFE** and blocks action. `BROKER READ FAILURE != ABSENCE`.

This contract is not implementable safely from current repository source because four necessary read paths and classifier/test artifacts are missing. The reported ten cases—active, trigger-pending, active conditional, filled, cancelled, rejected, expired, proven synthetic, malformed/insufficient evidence, and broker-read failure—must be imported as executable fixtures before Phase 2.

## 13. Risk engine

Non-protective entries are assessed at the last possible point under transaction locks. Implemented inputs/rules are:

| Rule | Input/calculation | Enforcement/rejection |
|---|---|---|
| global/user kill switch | persistent effective switch | deny new entry; protection remains allowed |
| permission and mode | active user, `can_live_trade`, demo/live/force-demo | deny/convert only under defined safe startup rules |
| broker session | connected token state for live | deny live submission |
| egress readiness | explicit inventory must be configured/verified/bindable | fail closed before broker access |
| market freshness | last exchange/token tick within configured max age | deny stale/missing price |
| reconciliation health | last broker read health and unresolved incident set | deny new exposure |
| lots/quantity | proposed plus reserved/current exposure against effective limits | persist deny reason |
| notional | normalized quantity × current price | compare global/user numeric limit |
| open positions | projected unique/open exposure count | deny over limit |
| daily trade count | existing plus reserved entry | deny over limit |
| realized/unrealized daily loss | current database positions/P&L against limits | deny over loss thresholds |
| duplicate position | user/exchange/token/open-trade/order exposure | block duplicate/contradictory entry |
| circuit limits | Angel full quote lower/upper circuit and normalized order price | reject outside/invalid quote |
| contract metadata | current lot and tick size | reject missing/stale/invalid contract |
| live margin | RMS available cash and Angel batch margin plus configured buffer | reject insufficient margin/read failure |
| strategy window | scheduler/candle/session-specific time rules | signal not emitted or intent expires/skips |

Decisions are normally persisted to `risk_decisions` with inputs/reason. Protective exits bypass exposure-creating limits and kill switches by design so the system can reduce risk, but they still require valid quantities, durable trade context and broker/account correctness. This is an intentional safety exception, not a kill-switch bypass. The migration must audit early-return branches to ensure every entry denial remains observable.

## 14. Demo/live safety and broker mutation inventory

| Mode | Broker read | Place | Modify | Cancel | Simulation |
|---|---:|---:|---:|---:|---:|
| demo | yes (session, instruments, quotes/candles/market data as needed) | no | no/not implemented | no for demo orders | yes, tick-driven; market may fill immediately |
| live | yes | yes | no/not implemented | yes | no |
| Python shadow (required) | allowlisted/read-only only | physically denied | absent/denied | physically denied | compare-only, never writing production trade truth |

`FORCE_DEMO_TRADING` is checked at startup and again immediately before entry submission. Startup changes live profiles to demo only where no open trade/nonterminal live order requires live protection. Existing retained live exposure may still require broker cancellation/protective mutation even under force-demo. Therefore force-demo is not a universal network firewall and cannot be the sole shadow defense.

Functions capable of live broker mutation:

- `angel::place_order` — **LIVE_BROKER_MUTATION**; reached from live entry, SL/target child creation, emergency close, square-off and reversal flows through `place_strategy_order_inner` or protection helpers.
- `angel::cancel_order` — **LIVE_BROKER_MUTATION**; reached by active-exit-role cancellation, supertrend cancellation/square-off, kill-switch cancellation, deactivation and reconciliation/protection cleanup.

No Modify/GTT/OCO mutation exists. HTTP endpoints that indirectly enable later writes (intent retry, activation) or immediate cancels (kill switch/deactivation) must be treated as mutation-capable even though their handlers do not call Angel Place directly.

## 15. Kill switch

Global and per-user switches persist in `risk_kill_switches` and are managed only by administrators with CSRF. Risk checks apply the effective global/user state to every new non-protective entry. Enabling marks relevant entry orders for cancellation and invokes broker cancellation for live nonterminal entries; the scheduler/reconciliation loop continues enforcing that state. Existing positions are not market-closed solely because a switch is enabled—SL/target, reconciliation and emergency risk-reduction remain active.

Potential bypass audit:

- protective exits intentionally proceed, because blocking them would increase risk;
- there is no public direct “place arbitrary order” endpoint;
- final live checks occur immediately before Angel submission, reducing time-of-check races;
- DB failure prevents normal execution rather than defaulting to allow;
- indirect admin retry/activation still passes normal worker risk checks.

Python parity tests must race switch activation against a claimed intent and prove no new entry reaches the broker after the locked final check.

## 16. Strategy inventory

> Historical timing summary. Current production behavior is reconciled in section 36.4, including the deployed SuperTrend 15:10 Asia/Kolkata cutoff and square-off.

### `futures_breakout_v3`

- Source: mostly `backend/src/strategy.rs`; backtest in `backtesting.rs`.
- Instruments: MCX `GOLDTEN`, `GOLDM`, `SILVERM`, `SILVERMIC`, `NATGASMINI`.
- Inputs/persistence: four prior-session high/low levels, opening range/gap plan, contract snapshot, per-user lots, day/evening activation, optional target/stop points.
- Schedule: day snapshot/entry/gap around 09:00/09:10/09:16 IST; evening around 17:00/17:10; bounded catch-up about 15 minutes. Exact exchange calendar/session code is authoritative.
- Entry: both BUY and SELL STOPLOSS_LIMIT breakout orders; guard against same-instrument open exposure and duplicate execution keys.
- Exit: target about 1.5% and stop formulas/tick normalization; TP1 split as above; SL2 runner can create opposite-direction reversal intent for original lots using fresh levels.
- Reliability: durable snapshot/scheduler/signal/intent/order/trade/reversal rows; broker ambiguity and partial-fill handling shared with core engine.

### `supertrend_index_options_v1`

- Source: `backend/src/strategy.rs`; no backtest implementation.
- Instruments: SENSEX and NIFTY underlying five-minute completed candles; Supertrend ATR period 7, factor 2.
- Entry: up flip buys ATM call; down flip buys ATM put; BFO/NFO contract resolution. Entry window begins 09:15 IST and forbids new entry at/after 15:20; intent freshness about 90 seconds.
- Exit/protection: close opposite exposure first; STOPLOSS_MARKET stop must be acknowledged before limit target. Defaults are reported in source as SENSEX 40 target/25 stop and NIFTY 25/15.
- Recovery: durable 15:20 square-off intent retries across restart; repeated schedule key prevents duplicates.

Legacy `option_entry_v1` behavior is retired. Some historical option/margin tables remain. Migration must not resurrect it or delete its history implicitly.

## 17. Reconciliation

> Historical reconciliation summary. Current production adds trade-book/conditional evidence, broker-side manual-close attribution, blockers, and revision-bound LIVE readiness as specified in section 36.4.

Every roughly five seconds, the scheduler builds an audience containing connected/eligible live accounts **or** users with nonterminal live orders, open live trades, or unresolved incidents. This ensures a disconnected user with exposure is still reconciled and alerted.

For each user, Rust reads Angel order book and positions. If order-book retrieval fails, health becomes unhealthy and the routine returns before treating any orders as absent. If positions fail, the same is true. This source trace verifies the invariant:

> **BROKER READ FAILURE != BROKER FLAT**

Order matching prefers broker order ID and stable client tag. Broker complete/filled, rejected and cancelled states advance the local order only through allowed transitions. Cumulative fills are monotonic and delta-accounted. Partial remainder cancellation remains nonterminal until observed. Ambiguous orders absent from a successful read wait for timeout and operator resolution; they are not retried blindly.

Positions are aggregated by exchange/token and compared to open local trades. The backend creates durable incidents for broker-only/unmapped positions, local-open/broker-flat state, quantity, direction and average-price mismatches. Such incidents block new entries and are resolved only after a later successful consistent observation. A manual cancel becomes known through order book; manual closure becomes a flat/mismatch reconciliation event rather than an assumed successful local exit. The code does not read Angel trade book or conditional/OCO state, and therefore cannot currently perform the reported classifier conjunction.

## 18. Workers and schedulers

> Historical worker summary. Section 36.5 is the current worker/concurrency contract.

The single scheduler leader wakes around every five seconds and performs startup recovery, contract warmup/refresh, scheduled futures dispatch, supertrend candle evaluation, due square-off, feed demand, entry shutdown, protection recovery, live reconciliation, reversal recovery, and intent worker spawning. Contract/snapshot refresh is about five minutes. Supertrend evaluation uses five-minute candles between 09:15 and 15:30, with entries stopped at 15:20. Square-off dispatch keys include minute identity to provide durable retry without duplicate execution.

Startup marks abandoned running scheduler records failed and moves stale claimed intents to retryable state. It then reconstructs protection/reversal/exposure from DB plus broker reads. Shared feeds reconnect and resubscribe independently. Browser streams are per connection. Broker session maintenance wakes every 60 seconds; a per-user mutex prevents duplicate refresh, failures defer about five minutes, and credential revision prevents stale token overwrite. App-session cleanup runs hourly.

Admin jobs include OTP cleanup, stale-session audit and strategy reload. The API labels imply schedules such as daily/30-minute, but source exposes them primarily as manually triggered spawned jobs; only session cleanup has a clearly automatic hourly loop. This UI/documentation mismatch is technical debt.

## 19. Concurrency guarantees

| Race | Current guarantee | Python parity requirement |
|---|---|---|
| two scheduler instances | PostgreSQL session advisory leader lock | acquire/hold equivalent dedicated-connection lock; health must expose follower state |
| simultaneous same signal | unique signal/scheduler keys and transactional fan-out | rely on same constraints; duplicate insert becomes idempotent result |
| two intent workers | `FOR UPDATE SKIP LOCKED`, claimed status, semaphore | DB claim transaction and bounded worker pool; stale claim recovery |
| risk check vs another entry/admin limit | global shared + per-user exclusive advisory locks and one transaction | identical lock ordering and projected reservation semantics |
| duplicate broker submission | durable order/idempotency/client tag before write; ambiguous state | never write before durable identity; never retry possible-delivery outcome |
| concurrent fills/reconcile | row locks, monotonic cumulative watermark, version/status trigger | one fill delta exactly once under lock |
| entry vs existing position | user/instrument serialization and duplicate exposure query | same DB-scoped serialization, not process lock |
| SL/target fill/cancel | terminal status confirmation, coverage trigger, recovery loop | do not infer sibling terminality; rebuild exact remaining coverage |
| double exit fill | durable over-close reconstruction and emergency close | preserve exact containment behavior |
| broker session refresh | per-user Tokio mutex + DB credential revision CAS | distributed ownership/CAS because two processes may exist |
| market-feed reconnect | feed registry locks, demand/subscription rebuild | one feed owner per key and deterministic resubscription |
| egress assignment/config | helper serial mutex + DB advisory locks + unique FK | coordinate address/user; block active-exposure changes or preserve old connection explicitly |
| manual broker closure | successful read plus durable incident/reconciliation plan | never translate missing/failed read to flat |

Tokio `Mutex` objects protect in-process registries and session/feed coordination; broadcast channels distribute ephemeral events; a global strategy semaphore bounds concurrent work. Python may use asyncio primitives for local resource control, but every correctness property that spans processes must remain in PostgreSQL or a deliberately single-owned service.

## 20. Crash and restart recovery

| Crash point | Durable evidence | Required restart behavior |
|---|---|---|
| before Angel entry write | pending/submitting order and intent | determine whether transmission was possible; only retry proven pre-delivery failure |
| during/after possible entry write | stable client tag + `ambiguous`/submitting state | read broker order book; never blind-place |
| after entry fill before trade update | broker cumulative fill plus order watermark | apply missing delta once under row lock |
| after trade update before SL | trade `PROTECTION_REQUIRED` | protection recovery submits exact uncovered slice |
| during SL submission | submitting/uncertain child + client tag | reconcile before creating another stop |
| after SL before target | stop broker acknowledgement/local state | create target only when coverage proof exists |
| during target/SL exit | cumulative fills, open quantity, safety state | reconcile both siblings and rebuild/contain remainder |
| during square-off/reversal | durable execution/reversal intent | recover claimed/waiting state and revalidate broker truth |
| broker WS loss | persisted latest ticks and demand registry | mark stale, block entries, reconnect/resubscribe; do not use stale price |
| reconciliation read failure | unhealthy record/error, prior incidents | preserve prior exposure; retry later; never mark flat |
| process restart | DB migrations, scheduler/intents/orders/trades/incidents | one leader reconstructs state before new entry work |
| host/backend restart with explicit IP | DB configured inventory | helper rehydrates aliases/routes/SNAT and verifies before workers |

HTTP graceful shutdown alone is not the safety mechanism. Durable state plus broker reconciliation is.

## 21. Retry, timeout, and idempotency

- HTTP clients use an overall 15-second timeout unless a narrower operation imposes its own bound.
- Angel reads may be retried/backed off under rate and response classification; historical candles are deliberately paced.
- Feed reconnect is exponential with jitter and a special longer rate-limit delay.
- Session-refresh failures defer subsequent attempts and use revision CAS.
- Protection acknowledgement defaults to 30 seconds and up to three controlled attempts; uncertain child writes reconcile before retry.
- Ambiguous entry timeout defaults to 120 seconds and ends in incident/operator attention, not automatic place retry.
- Stable database idempotency keys, Angel client tags, scheduler execution keys, signal uniqueness and execution-intent partial unique indexes cover separate duplicate surfaces.
- Cancel retry is safe only after examining current broker/local terminal status; a cancel response does not erase fill races.
- Rejected/auth/validation responses are non-retryable until cause/state changes. Rate limits honor cooldown/`Retry-After`. Transport errors proven before delivery can return to a controlled retry state.

Every Python retry policy must answer: “Can the previous request have reached Angel?” If yes or unknown, the only safe next action is read/reconcile.

## 22. Frontend compatibility

The current React client expects same-origin `/api` routing, cookie credentials, readable CSRF cookie plus `X-CSRF-Token`, trailing slashes on several auth/home/log/job routes, JSON `detail`, and 401-driven auth reset. Keep UUIDs and timestamps serialized as strings; timestamps remain RFC 3339/UTC unless an endpoint explicitly presents IST-derived dates. Keep snake_case JSON keys, string status values, pagination fields and XLSX content disposition.

The first Python backend should preserve all routes and method semantics, including the unusual admin user collection mutations. Do not “clean up” route naming, switch to bearer JWTs, rename enums, change null to omitted, or change numeric JSON formatting without recorded compatibility tests. Both WebSocket endpoints must accept session-cookie authentication during upgrade. Because current React source does not appear to open them, protocol fixtures rather than UI tests must establish compatibility.

## 23. Test inventory and gaps

> Historical test checkpoint. Section 36.8 records the exact current-release rerun and resolves the OCO provenance gap.

### Repository coverage

- Rust unit tests cover security/config, Angel parsing/error handling/rate behavior, strategy calculations/state transitions, risk/order correctness, demo/live protections, reconciliation and egress address mapping.
- Stateful PostgreSQL safety tests are explicitly `#[ignore]` and refuse any database except loopback with a `rulenix_test_*` name. They exercise migrations, locking, transition triggers, fill/recovery and concurrency properties.
- Deterministic fake broker routes in `strategy.rs` cover order book, positions, RMS, margin, place and cancel behavior without Angel.
- Production egress release adds a stable public-to-private alias unit test. Helper integration/network namespace behavior needs privileged environment tests.
- Frontend Vitest coverage spans auth/layout, admin users/egress, account and strategy/admin flows.
- Live Angel and production environment checks are manual/gated and were not run in this audit.

The migration brief reports Rust 129 passed, OCO classifier 10/10, and frontend 22 passed. A disposable, isolated extraction of exact production commit `09a03a1…` produced `129 passed; 0 failed; 20 ignored` for `cargo test --all-targets`, verifying the Rust release count without changing the checkout. Safe validation against the current dirty working tree separately produced `131 passed; 0 failed; 21 ignored`, showing that it is not the same source set. The current frontend produced `22 passed` across eight files. The **OCO 10/10 cannot be verified**: no classifier or test source exists in Git or the worktree backend.

### Missing critical coverage

1. source-controlled OCO classifier with raw redacted broker fixtures and the ten reported cases;
2. read-failure versus empty order/position/trade/conditional responses across every reconciliation branch;
3. exact production helper namespace/route/SNAT integration and startup rehydration failure;
4. assignment change while an Angel WebSocket or live exposure exists;
5. Rust/Python cross-process contention for scheduler/intents/risk locks;
6. deterministic crash injection before/after every live write and fill/protection transaction boundary;
7. property/golden tests for Decimal/f64/tick rounding, partial fills and both BUY/SELL reversals;
8. API schema snapshots and browser WebSocket protocol fixtures;
9. kill-switch race at the final submission gate;
10. prove every shadow image lacks network/credential capability for broker mutation.

## 24. Production deployment and future shadow topology

`docker-compose.prod.yml` runs PostgreSQL 16 Alpine with health checks, the Rust backend, and an Nginx frontend bound locally behind Caddy/public TLS. The backend image is read-only with explicit log/CA/helper-socket mounts and dropped capabilities. PostgreSQL uses TLS verification. Backend startup applies migrations under its advisory lock. The privileged egress helper is a separate root systemd service; an older standalone backend systemd unit also exists and must be classified as active or stale before cutover to avoid a duplicate server/worker.

CI runs Rust formatting, Clippy, tests, migration filename checks and `cargo audit`; frontend install/lint/test/build/audit; and secret scanning. Operational scripts cover encrypted PostgreSQL backup, restore, deployment/readiness and load smoke checks. Rollback must preserve the Rust image, exact config, helper and database compatibility; schema changes after cutover must remain backward-compatible until rollback retention expires.

Future shadow topology:

```text
production traffic ─> Rust (only writer / scheduler / broker mutation authority)
                 └─ mirrored sanitized request/event envelope ─> Python shadow
PostgreSQL primary ─> read-only role or replica ────────────────> Python shadow
Angel read proxy ──> allowlisted GET/read POST only ───────────> Python shadow
Python comparison output ─> separate schema/log sink (never production domain tables)
```

Shadow must have a different deployment identity, no helper socket, no production credential decryption keys unless strictly needed behind a read proxy, no write-capable DB role, and network policy denying Angel mutation endpoints.

## 25. Target Python architecture (design only)

Use Python 3.12+, FastAPI, Pydantic v2, SQLAlchemy 2 async with asyncpg, httpx, `websockets` (or httpx-compatible async WS library with explicit socket bind support), asyncio, Alembic for future migrations, pytest/pytest-asyncio, `cryptography` AESGCM, `argon2-cffi`, structlog/standard JSON logging, and openpyxl/xlsxwriter for exports. Raw SQL should remain where lock semantics or complex set operations make ORM translation unsafe.

```text
python-backend/                         # create only in Phase 2
├── pyproject.toml
├── src/rulenix/
│   ├── app.py                          FastAPI construction/lifespan
│   ├── config.py                       typed settings + production validation
│   ├── api/                            routers and Pydantic wire models
│   │   ├── auth.py account.py home.py admin.py
│   │   ├── pnl.py backtesting.py risk.py strategy.py
│   │   ├── egress.py logs.py jobs.py health.py websockets.py
│   │   └── errors.py
│   ├── db/                             engine, existing-schema models, raw queries,
│   │   ├── locks.py                    advisory/row-lock protocols
│   │   └── migrations.py               compatibility/version checks only initially
│   ├── security/                       sessions, CSRF, password, OTP, crypto
│   ├── angel/                          credentials, session, REST read/write split,
│   │   ├── readonly.py mutations.py errors.py rates.py
│   │   ├── websocket.py egress.py
│   │   └── mutation_capability.py
│   ├── trading/                        domain/status/rounding/order/protection/fills
│   ├── risk/                           evaluation, reservations, kill switch
│   ├── strategies/                     futures_breakout, supertrend_options
│   ├── reconciliation/                 plans, incidents, OCO classifier after artifact
│   ├── workers/                        leader, scheduler, intents, recovery, sessions
│   ├── observability/                  audit, alerts, metrics, redaction
│   └── shadow/                         comparison-only sinks and capability guard
└── tests/
    ├── contract/ parity/ golden/ unit/
    ├── postgres/ broker_fixtures/ concurrency/ crash/
    └── shadow_capability/
```

Keep the root helper in Rust initially or replace it only with a separately reviewed least-privilege helper. Python's Angel layer must expose read-only and mutation interfaces as different types/dependencies; mutation capability is injected only into the active live executor image.

## 26. Rust to Python mapping

| Rust component | Rust path | Responsibility | Python destination | Risk | Phase |
|---|---|---|---|---|---:|
| config/startup | `main.rs`, `config.rs` | lifecycle, validation, routing | `app.py`, `config.py` | HIGH | 2 |
| DB/migrations | `migrations`, raw SQL across modules | durable schema/locks | `db/` existing-schema mapping | CRITICAL | 2,5–8 |
| auth/security | `auth.rs`, `security.rs` | users, OTP, sessions, CSRF | `api/auth.py`, `security/` | HIGH | 2 |
| credential store | `credentials.rs` | encrypted broker tokens | `security/crypto.py`, `angel/credentials.py` | CRITICAL | 2–3 |
| admin/account/home | `auth.rs`, `account.rs`, `home.rs` | permissions/profile/connect/mode | corresponding `api/` routers/services | HIGH | 2–3 |
| Angel REST | `angel.rs` | reads/place/cancel/session | `angel/readonly.py`, `mutations.py` | CRITICAL | 3 |
| Angel/browser WS | `market_ws.rs` | market stream/binding | `angel/websocket.py`, `api/websockets.py` | HIGH | 3 |
| egress application | `egress.rs` | select/verify/bind/admin | `angel/egress.py`, `api/egress.py` | CRITICAL | 4 |
| egress helper | `bin/rulenix-egress-helper.rs` | privileged host networking | retain standalone helper initially | CRITICAL | 4/later |
| trading model/state | `models.rs`, migrations, `strategy.rs` | orders/trades/fills/protection | `trading/` | CRITICAL | 5 |
| risk/kill | `risk.rs` | limits, reserve, cancellation | `risk/` | CRITICAL | 6 |
| strategies | `strategy.rs` | signal and exit logic | `strategies/` | CRITICAL | 7 |
| reconciliation/recovery | `strategy.rs` | broker truth and repair | `reconciliation/`, `workers/recovery.py` | CRITICAL | 8 |
| scheduler/workers | `main.rs`, `strategy.rs` | leader/timers/intents | `workers/` | CRITICAL | 8 |
| backtesting | `backtesting.rs` | candle simulation/export | `api/backtesting.py`, service | MEDIUM | 9 |
| P&L/logs/jobs | respective modules | remaining APIs | respective `api/` services | MEDIUM | 9 |
| audit/alerts/errors | `audit.rs`, `error.rs` | evidence and public failures | `observability/`, `api/errors.py` | HIGH | 2,9 |
| frontend contracts | `frontend/src` | consumer behavior | contract suite, minimal React changes | HIGH | 9–10 |

## 27. Numeric and time semantics

- PostgreSQL prices, P&L, limits and margin values are commonly `NUMERIC`, often scale 2; quantities/lots are integers.
- Rust frequently converts numeric values and broker JSON to `f64`. Strategy percentages, Supertrend/ATR, circuit checks and P&L therefore inherit IEEE-754 behavior before explicit price/tick normalization.
- Python must use `Decimal` at API, DB and broker boundaries and exact integer quantity. It must not silently change strategy outputs by replacing every intermediate f64 computation with arbitrary Decimal semantics.
- Build golden vectors from Rust for raw calculation, side-aware tick rounding, 2-decimal storage, target/stop prices, notional/margin, partial realized P&L and reversal quantities. Initially reproduce the Rust result at each persisted/broker boundary; propose intentional numeric corrections separately.
- Dates/schedules use IST (`Asia/Kolkata`) for trading sessions and UTC `timestamptz`/RFC 3339 for durable instants. DST is not present in IST but timezone-aware datetimes are mandatory.
- UUIDs remain canonical lower-case strings on the wire and native UUID in DB. Rust `Option` maps to SQL NULL/JSON null according to each existing response, not automatic omission.
- Preserve exact status strings and check constraints; use explicit Python `StrEnum` only where unknown-value failure remains visible.

## 28. BEHAVIORAL PARITY CONTRACT

Rust and Python are equivalent only when the following observable contracts pass against identical fixtures and database state:

1. **API:** identical route/method/trailing-slash acceptance, cookie/CSRF behavior, authorization, request rejection, JSON keys/nulls/status/error envelope, pagination, exports and WS upgrade behavior.
2. **Database:** identical existing-table reads/writes, transaction boundaries, status transitions, lock ordering, uniqueness/idempotency behavior, audit records and failure rollback.
3. **Auth:** equivalent password policy/Argon2 verification, OTP timing/attempt semantics, enumeration resistance, session idle/absolute/password fence, permission and live-confirmation checks.
4. **Strategies:** identical completed-candle selection, schedules/catch-up, contracts, levels, signals, sides, lots/quantity, entry/expiry, SL/target/reversal and square-off decisions for golden histories.
5. **Calculations:** same tick/price/percentage/margin/P&L outputs at broker and persisted boundaries, including negative and half-tick cases.
6. **Orders:** identical durable identity before write, Angel payload, client tag, status transition, rejected/retryable/ambiguous handling, cumulative fill delta and terminality.
7. **Protection:** stop-before-target acknowledgement, exact partial coverage, sibling cancel/fill races, protection recovery and over-close emergency containment.
8. **Reconciliation:** same broker ID/tag mapping, health/incidents, manual cancel/closure behavior and absolute rule that read failure is never flat/empty evidence.
9. **OCO:** only the source-controlled strict conjunction may classify the known synthetic record; every missing/error/extra-executable case is UNKNOWN and blocks.
10. **Demo/live:** demo performs no broker order mutation; live requires all gates; force-demo preserves management of existing live exposure; mutation inventory is complete.
11. **Kill switch:** new entries stop at the final locked gate; entry cancels are enforced; existing protection and risk-reducing exits continue.
12. **Egress:** NULL remains unbound default networking; explicit X produces traffic from X for REST and WS; unavailable explicit selection never falls back; account/source identity cannot cross.
13. **Recovery:** every injected crash point converges from DB+broker truth without duplicate exposure, missing fill, unprotected position or hidden incident.
14. **Scheduling:** same IST calendar/window, durable execution keys, one leader, due/retry/expiry decisions and no Rust/Python double ownership.

Parity comparison must distinguish “same expected decision” from “same broker mutation.” Until cutover, Python is allowed to record the former and forbidden to perform the latter.

## 29. Required behavior versus suspected defects

### Required behavior

Everything in the parity contract, particularly fail-closed broker reads/egress, durable write identity, ambiguous-order reconciliation, protection ordering, demo/live gates, kill-switch exceptions for risk reduction, and the existing wire/schema contracts.

### Suspected defect / technical debt

| Evidence | Risk | Treatment / correction phase |
|---|---|---|
| OCO classifier/tests reported but absent from source; required Angel read APIs absent | CRITICAL unverifiable production behavior | block Phase 2; recover exact artifact and fixtures before implementation |
| checked-out master/worktree differs from production and contains stale egress copies | CRITICAL wrong source translation | establish clean signed/tagged migration baseline before Phase 2 |
| `strategy.rs` is ~13.7k lines with many responsibilities | HIGH accidental coupling | decompose behind characterization tests in Phases 5–8, no behavior change |
| broad Rust `f64` use against PostgreSQL NUMERIC | HIGH rounding/parity risk | golden parity first; intentional Decimal correction after cutover approval |
| egress assignment does not close old WS or visibly block active exposure | HIGH wrong source IP after admin change | operational prohibition now; designed reconnect/drain change after parity |
| shared strategy feed selects one eligible user's Angel session | HIGH account availability/isolation ambiguity | document selection and add account/IP tests in Phase 3; redesign only separately |
| explicit REST client constructed per operation | MEDIUM latency/rate/resource behavior | preserve observable isolation; later safe cache keyed solely by binding identity |
| metrics seems available to any authenticated user despite admin/internal description | MEDIUM information exposure | preserve initially or explicitly security-fix with frontend/ops approval in Phase 9/10 |
| admin job labels imply automatic cadence absent from source | LOW/MEDIUM operations mismatch | clarify source of schedule in Phase 9; do not invent timers |
| graceful shutdown does not explicitly drain individual workers | HIGH cutover/in-flight ambiguity | rely on durable recovery initially; add lease/drain protocol before cutover |
| legacy tables/migrations and alternative systemd unit remain | MEDIUM accidental activation/deletion | inventory active deployment; do not remove until rollback retention ends |

No suspected defect may be silently fixed in translation. Capture Rust golden behavior, open a separate change decision, and test both safety improvement and compatibility impact.

## 30. Migration risk register

| Risk | Severity | Rust protection | Python requirement | Future verification |
|---|---|---|---|---|
| duplicate live orders | CRITICAL | durable order/tag, ambiguity state, reconcile | durable-before-write; no possible-delivery retry | crash/fault injection at socket boundaries |
| BUY/SELL reversal error | CRITICAL | serialized exposure accounting, durable reversal | exact side/qty/fresh-level parity | golden and concurrent reversal cases |
| quantity mismatch | CRITICAL | live lot metadata, integer qty, fill watermark | exact lot/remaining/protection arithmetic | property tests and broker fixtures |
| price rounding | HIGH | tick normalization/f64 behavior | boundary Decimal with Rust goldens | half-tick/negative/large vector suite |
| SL mismatch/unprotected fill | CRITICAL | stop-first ack, recovery, coverage trigger | exact slice coverage and recovery | crash after each protection step |
| target mismatch | CRITICAL | persisted target/splits, reconciliation | identical target/split/remaining rules | multi-lot/partial golden tests |
| partial fills | CRITICAL | cumulative watermark + row lock | delta exactly once | duplicate/out-of-order event tests |
| unknown broker write | CRITICAL | ambiguous, tag lookup, no retry | same tri-state outcome model | lost-response fake broker |
| broker read failure | CRITICAL | unhealthy return, no flat inference | typed read failure, retain exposure | error-vs-empty fixtures |
| synthetic OCO | CRITICAL | reported strict classifier only | source-backed conjunction; unknown blocks | missing 10 cases plus adversarial variants |
| WebSocket loss/stale tick | HIGH | heartbeat/staleness/reconnect | block new entry until fresh; resubscribe | disconnect/reorder simulation |
| restart during trade | CRITICAL | durable state/recovery | reconstruct from DB+broker | kill process at transaction boundaries |
| scheduler duplication | CRITICAL | advisory leader/unique keys | one leader, same keys | two-process contention test |
| Rust/Python overlap | CRITICAL | not currently designed for mixed writers | hard single-writer lease/capability | deployment invariant test |
| wrong Angel account | CRITICAL | per-user credential load/revision | typed account context through every call | cross-user concurrency fixtures |
| wrong source IP | CRITICAL | per-user selection/bound socket/fail-close | preserve account→alias binding | REST/WS observed-IP integration |
| cross-account HTTP pool leakage | HIGH | explicit per-call client; global NULL pool no credentials | pool key includes source/account isolation as needed | concurrent server connection tracing |
| demo reaches live | CRITICAL | mode/final checks/simulator split | no mutation capability in demo/shadow | network-deny and call-graph tests |
| kill-switch bypass | CRITICAL | locked final check; cancel entries | same ordering, only risk-reducing exception | activation race test |
| DB race/deadlock | CRITICAL | ordered advisory/row locks, constraints | reproduce lock order and retries | multi-process stress/deadlock tests |
| secret leakage | CRITICAL | AES-GCM/AAD/redaction/zeroize | compatible crypto, typed redaction | logs/errors/crash-dump scans |
| manual broker action | HIGH | order/position reconciliation/incidents | never claim ownership without mapping | manual cancel/close/unknown fixtures |
| helper compromise | CRITICAL | narrow root socket/service/capabilities | retain narrow protocol; no shell passthrough | threat model and privileged integration audit |

## 31. Shadow mode design

Shadow safety must be layered and mechanically testable:

1. build a separate image without `angel/mutations.py` registered and without worker entry points;
2. use a PostgreSQL role with transaction/read-only enforcement and no INSERT/UPDATE/DELETE on production schema; comparisons go to a separate isolated store;
3. provide no egress-helper socket and no Linux networking capabilities;
4. provide no live broker refresh/credential write permission; preferably provide read data through a Rust/read proxy with an explicit operation allowlist;
5. enforce outbound network policy/proxy rules that allow only known read endpoints and deny Place, Cancel, Modify, GTT/OCO paths regardless of HTTP method;
6. require a cryptographic/deployment “mutation capability” object to construct a live Angel writer; shadow has no such secret or dependency;
7. disable scheduler leader acquisition, intent claims, reconciliation writes, demo fills, protection, square-off, reversal, session refresh and admin mutation routes;
8. expose startup self-test evidence showing DB read-only, helper absent, mutation routes unresolvable and broker mutation network probes denied;
9. alert and terminate on any attempted mutation instead of returning a fake success.

Shadow inputs should be sanitized request envelopes, DB snapshots/read replica rows, Rust decisions/events and allowlisted broker read fixtures. Comparison keys must include user/account, strategy/instrument/session, database snapshot/version, config version, tick/candle identity and source-IP mode so differences are attributable.

## 32. Official migration sequence

1. Production Rust audit + migration specification
2. Python foundation + PostgreSQL + Auth/Admin/Config
3. Angel REST + WebSocket
4. Per-user static egress IP
5. Trading domain + order state machine
6. Risk + safety engine
7. Strategy + execution engine
8. Reconciliation + recovery + workers
9. Remaining APIs + frontend compatibility
10. Rust/Python parity framework + full test/security audit
11. Production shadow deployment + parity observation
12. Python demo trial + production cutover preflight
13. Production cutover + verification + Rust rollback retention

Phase 2 must not begin until the blockers below are resolved and the authoritative production source set is frozen.

## 33. Migration checkpoint

> Historical blocked checkpoint. Sections 34-36 record the later committed/deployed source that resolved its production-provenance blocker; section 36 is authoritative.

```text
CURRENT PHASE: 1
PHASE STATUS: BLOCKED
CURRENT RUST COMMIT: e75681792584c8e0eee61ecf576247e9d8771005 (local checked-out master)
CURRENT PYTHON MIGRATION COMMIT: NONE — PYTHON NOT STARTED
PRODUCTION RELEASE: 09a03a1d2267773afac4b719f4ab1ab239588aaf
PRODUCTION MODIFIED: NO

COMPLETED:
- repository, production release object, runtime architecture, API, WS, DB, auth and config audit
- Angel REST, trading, risk, demo/live, kill switch, strategy, reconciliation and worker trace
- production egress source/helper/container networking trace
- Python target design, component mapping, parity contract, risk register and shadow design
- safe exact-production-release validation: Rust 129 passed, 0 failed, 20 ignored
- safe current-worktree validation: Rust 131 passed, 0 failed, 21 ignored; frontend 22 passed
- formatting, frontend lint and production frontend build checks passed

DISCOVERED:
- production Rust backend is approximately 25,853 lines in 26 source files
- 48 ordinary HTTP contracts plus two authenticated browser WebSocket endpoints
- 32 PostgreSQL tables and 47 production migrations
- only Angel Place Order and Cancel Order are implemented live broker mutation operations
- NULL egress is intentionally unbound; explicit public IP uses a private alias/return route/SNAT
- broker-read failure paths preserve unknown exposure instead of treating it as flat
- reported OCO classifier and required broker read functions are absent from repository source
- local master/dirty worktree is not identical to the named production release

OPEN QUESTIONS:
- Where is the exact deployed OCO classifier source, its deployment commit, redacted fixtures, and 10-test suite?
- Which exact individual-order, trade-book and conditional-inventory Angel endpoints/headers/parsers did it use?
- Is the old standalone backend systemd unit disabled on production?
- What operational control prevents egress reassignment while an old WS/live position is active?
- Which account-selection policy is intended for shared strategy market feeds?

KNOWN RISKS:
- duplicate live writes, ambiguous outcomes, partial/double fills and unprotected exposure
- wrong account/source IP, Rust/Python worker overlap, demo/shadow reaching mutation paths
- numeric rounding drift, scheduler duplication, read-failure misclassification, secret leakage
- source/runtime provenance drift and undocumented OCO behavior

BLOCKERS:
- OCO classifier/test/read-client implementation claimed as production behavior is not source-controlled or auditable
- a clean canonical migration baseline combining the named production release with any later deployed OCO artifact has not been identified

NEXT PHASE:
2 — Python Foundation + PostgreSQL + Auth/Admin/Config
```

Phase 1 documentation is complete, but readiness is blocked by missing safety-critical production provenance. Do not start Phase 2 from the current dirty master or reconstruct the OCO classifier from this prose.

## 34. Superseding Rust production baseline (2026-09-05)

This section supersedes the older production-release and provenance checkpoint above. It records the deployed Rust baseline only; Python implementation remains explicitly not started.

### Authoritative release

- Production branch: `angel-egress-release`
- Production release: `8a791ea79b96d13015eaac2a66aaaa29b64bab0d`
- Required ancestor/combined feature candidate: `b67e889d488bbd57181ca56c2f75c62e34e4d109`
- Latest successful migration: `20260904010000_broker_deployment_readiness.sql`
- PostgreSQL migration state: 49 successful, 0 failed
- Internal and public readiness: healthy/ready
- Global Kill Switch after deployment: restored to its exact pre-deployment state (`false`); restoration has an append-only audit event

### Features in the new Rust baseline

- audited admin Global Kill Switch enable/disable control, with new LIVE entry suppression and risk-reducing protection/close behavior preserved;
- Future Breakout confirmed-SL2-fill reversal: exactly one durable opposite-side reversal (`BUY` to `SELL`, `SELL` to `BUY`) through the existing idempotent execution pipeline;
- broker-aware Manual Close Trade for running trades, including durable intent ownership, partial fills, duplicate submissions, ambiguous writes, protection races, restart recovery, and reconciliation;
- detection and reconciliation of LIVE positions closed manually in Angel;
- per-user Angel static egress and complete read-only positions, order-book, trade-book, individual-order, and conditional/GTT reconciliation;
- the strict informational/synthetic OCO classifier and its source-controlled tests;
- deployment readiness that permits an offline inactive account only when durable Rulenix state has no known or unresolved LIVE exposure, while keeping that account ineligible for new LIVE exposure.

### Exact deployment/readiness rule

Broker read failure is never broker-flat. For every Rulenix user, the deployment gate combines an authoritative broker read when available with the durable `broker_deployment_account_safety` inventory. A readable account passes only when positions, exposure-capable/unknown orders, trade evidence, and active conditional rules are safe. An unreadable account may pass deployment only when all eight durable classes are zero: open LIVE trades; unresolved closed LIVE trades/nonzero broker net; nonterminal or ambiguous LIVE orders; pending/claimed/retry/submitted LIVE execution intents; unresolved LIVE reversals; incomplete LIVE manual-close intents; open/operator-required broker-position incidents; and unresolved broker-mutation blockers (including externally discovered active/unknown orders and active conditional rules). Any nonzero class blocks deployment.

An unreadable account that is allowed through deployment remains `broker_reconciliation_health.healthy=false` and cannot create new LIVE exposure. Credential/session revision changes invalidate prior health. Reconnect triggers fresh positions, orders, trade-book, individual-order as needed, and full conditional/GTT reconciliation through the account's assigned egress. LIVE readiness returns only after that authoritative reconciliation succeeds, matches the current credential revision, is fresh, and finds/reconciles no unsafe exposure. A failed or partial read retains unknown/unreconciled state and never clears blockers as though the broker were flat.

### Verification evidence

- Rust ordinary suite: 130 passed, 0 failed, 28 ignored.
- PostgreSQL/stateful suite: 28 passed, 0 failed.
- Frontend Vitest: 28 passed; lint and production build passed.
- Broker gate/OCO classifier Node suite: 19 passed, 0 failed.
- `cargo fmt --check`, `cargo check`, Clippy with warnings denied, and Rust release build passed.
- Encrypted production backup, restore rehearsal, and migration preflight passed before cutover.
- Post-deployment schema/state, egress routing, authorization, OCO/gate behavior, and internal/public health checks passed.
- Final durable production state: 0 open LIVE trades, 0 active LIVE orders, 0 unresolved durable gate blockers.
- Deployment verification broker mutations: placed 0, modified 0, cancelled 0.
- No production demo trades were administratively cleared and no user was switched to LIVE.

```text
CURRENT PHASE: 1
PHASE STATUS: RUST BASELINE DEPLOYED; PYTHON NOT STARTED
AUTHORITATIVE RUST PRODUCTION RELEASE: 8a791ea79b96d13015eaac2a66aaaa29b64bab0d
CURRENT PYTHON MIGRATION COMMIT: NONE - PYTHON NOT STARTED
NEXT ACTION: re-baseline Phase 2 planning against this exact Rust release; do not implement Python without a separate authorization
```

## 35. Scoped admin cleanup and SuperTrend 15:10 Rust baseline (2026-09-05)

This section supersedes section 34 as the authoritative deployed Rust baseline. It records completed Rust work only; no Python source or migration implementation was started.

### Authoritative release

- Production branch: `angel-egress-release`
- Combined feature commit: `0b5461f9a11b74d1f75148f9348cd3ee6e84f1a3`
- Final deployed descendant: `3f788f2a842ef9b1b66366d439431867850e3753`
- Required ancestors: `8a791ea79b96d13015eaac2a66aaaa29b64bab0d` and `b67e889d488bbd57181ca56c2f75c62e34e4d109`
- Latest successful migration: `20260904010000_broker_deployment_readiness.sql`
- PostgreSQL migration state: 49 successful, 0 failed; this release adds no migration
- Internal and public readiness: healthy/ready
- Global Kill Switch after deployment: preserved exactly as found before deployment (`true`)

### Added Rust production behavior

- Admin user-history cleanup now has explicit `DEMO`, `LIVE`, and `ALL` scopes. All scopes require the Global Kill Switch to be enabled and execute under global/user locks with an atomic append-only audit event.
- `LIVE` and `ALL` cleanup first require zero durable LIVE/unresolved state, then authoritative read-only Angel positions, order-book, trade-book, and full conditional/GTT reconciliation through the user's assigned egress, followed by a second durable-state check. Read failure, unknown response, broker exposure, nonterminal/unknown orders, or active conditional rules fail closed.
- Cleanup never places, modifies, cancels, or closes a broker order. `ALL` performs LIVE safety verification before deleting any DEMO records, preventing partial unsafe cleanup. User identity, broker credentials, profiles, permissions, egress inventory/assignment, strategy configuration/activation, reconciliation incidents/blockers/health, and prior audit history remain preserved.
- SuperTrend Index Options now stops new entries at 15:10 IST and sends the existing idempotent, risk-reducing EOD close pipeline at 15:10 IST. The 15:30 watchdog remains unchanged. Futures Breakout's separate 15:20 expiry behavior, confirmed-SL2 reversal, manual close, OCO, kill-switch, and egress semantics remain unchanged.
- Production release exports now enforce LF endings for Linux `*.sh` operational scripts. This fixes a post-cutover safety-gate execution defect detected during the first audit of `0b5461f`; the final descendant artifact was re-backed-up, restore-tested, redeployed, and audited successfully.

### Deployment/readiness rule retained

`BROKER READ FAILURE != BROKER FLAT`. A broker-unreadable account may permit a platform deployment only when all durable safety classes are zero: open LIVE trades; unresolved closed LIVE trades/nonzero broker net; nonterminal or ambiguous LIVE orders; unresolved LIVE execution intents; unresolved reversals; incomplete manual-close intents; open/operator-required broker incidents; and unresolved broker-mutation/reconciliation blockers. Any nonzero class blocks deployment.

An unreadable account remains `live_ready=false` even when deployment is allowed. New LIVE exposure is prohibited until reconnect triggers a complete authoritative positions, orders, trade-book, individual-order where required, and conditional/GTT reconciliation for the current credential revision through the assigned egress. Only successful fresh reconciliation with no unresolved unsafe exposure restores LIVE entry eligibility; partial or failed reads never erase exposure or blockers.

### Verification and production evidence

- Rust ordinary suite: 131 passed, 0 failed, 31 ignored.
- PostgreSQL/stateful fake-broker suite: 31 passed, 0 failed.
- Frontend Vitest: 29 passed across 9 files; lint and production build passed.
- Broker deployment-gate/OCO classifier suite: 19 passed, 0 failed.
- `cargo fmt --check`, `cargo check --all-targets`, Clippy with warnings denied, and `cargo build --release --all-targets` passed.
- Fresh encrypted production backup and full restore/migration preflight passed; preserved snapshot counts were `4|3|64|786|220|2|11|0|0|0`.
- Final production state: 64 trades, 786 strategy orders, 0 open DEMO trades, 0 open LIVE trades, 0 active LIVE orders, and 0 durable deployment blockers.
- Four Angel accounts were offline/unreadable, each with zero durable unresolved LIVE state. Deployment passed without requiring reconnection; every account remained LIVE-blocked pending fresh authoritative reconciliation.
- Egress helper/socket, default route, both public source IP paths, auth boundaries, migration state, backend logs, and internal/public readiness passed post-deployment checks.
- No trade history was cleared during deployment, no user was switched to LIVE, and no real trade was manipulated.
- Deployment and verification broker mutations: placed 0, modified 0, cancelled 0.

```text
CURRENT PHASE: 1
PHASE STATUS: RUST BASELINE DEPLOYED; PYTHON NOT STARTED
AUTHORITATIVE RUST PRODUCTION RELEASE: 3f788f2a842ef9b1b66366d439431867850e3753
CURRENT PYTHON MIGRATION COMMIT: NONE - PYTHON NOT STARTED
NEXT ACTION: re-baseline Phase 2 planning against this exact Rust release; do not implement Python without separate authorization
```

## 36. Phase 1A current-production reconciliation (2026-09-06)

This section is the current authoritative migration checkpoint and supersedes stale facts in sections 0, 3, 5, 8, 12, 16-18, 23, 33, and 34. Correct architectural material in those sections remains part of the specification. This reconciliation changed documentation only; it did not deploy code, change production data, contact an Angel trading mutation API, or start Python work.

### 36.1 Authoritative baseline and source reproduction

- `CURRENT_AUTHORITATIVE_RUST_PRODUCTION_COMMIT`: `3f788f2a842ef9b1b66366d439431867850e3753`.
- Direct read-only production verification on 2026-09-06: `/opt/rulenix/RELEASE_COMMIT` returned that exact SHA; public `/api/health/ready` returned `{"checks":{"database":"ok"},"status":"ready"}`.
- Production branch/source branch: `angel-egress-release`; its dedicated worktree is clean at the exact production SHA.
- The ordinary local worktree remains on `master` at `e75681792584c8e0eee61ecf576247e9d8771005`, ahead of `origin/master` and intentionally dirty with pre-existing changes. It was not reset, cleaned, or used as production source.
- Git object verification: the production SHA is a commit, its complete 195-file tracked tree is present, the old audit baseline is its ancestor, and `git archive` embeds the same commit ID. The release archive `.runlogs/deploy-3f788f2.tar.gz` has marker `3f788f2...` and SHA-256 `be76045ad2db4f188639f61fbf9371783c4d97f7f32f8744ecb82062bb7a53f2`.
- Current source size is approximately 27,931 Rust lines in 26 files; `strategy.rs` is approximately 15,507 lines.
- Direct read-only production database verification: 49 successful migrations, zero failed, latest `20260904010000`; 37 public base tables and one public view.
- Direct current safety snapshot: Global Kill Switch enabled; zero open LIVE trades; zero active/nonterminal LIVE orders; zero rows with nonzero durable deployment blockers.
- Direct current egress snapshot: `139.99.155.62` and `51.161.140.103` are both `CONFIGURED/VERIFIED`; each currently has one explicit account assignment. One reconciliation-health row is healthy and one is unhealthy. An unhealthy/offline row is not LIVE-ready.

The previous migration audit baseline was `09a03a1d2267773afac4b719f4ab1ab239588aaf`. Five commits were added between that baseline and current production:

| Commit | Production-relevant delta |
|---|---|
| `904a6452a45a81152724db54dadc457cddd56f19` | Audited Admin Global Kill Switch read/update contract and final entry cancellation behavior |
| `b67e889d488bbd57181ca56c2f75c62e34e4d109` | Safe two-way SL2 reversal, user Manual Close Trade, broker-side manual-close reconciliation, trade/conditional broker reads, and two migrations |
| `8a791ea79b96d13015eaac2a66aaaa29b64bab0d` | Offline-account deployment decision and revision-bound LIVE readiness |
| `0b5461f9a11b74d1f75148f9348cd3ee6e84f1a3` | Explicit DEMO/LIVE/ALL Clear Trades and SuperTrend 15:10 IST entry cutoff/EOD close |
| `3f788f2a842ef9b1b66366d439431867850e3753` | LF-enforced production shell-script export; no application/schema behavior change |

### 36.2 Current API and WebSocket delta

The complete section 3 inventory remains valid except where superseded here. Current totals are **50 ordinary HTTP method/route contracts plus two authenticated browser WebSocket upgrades across 46 unique paths**. All existing cookie-session, CSRF, authorization, JSON/error-envelope, trailing-slash, and frontend compatibility rules remain unchanged.

| Contract | Authentication/authorization | Request and response | DB/broker effects and errors |
|---|---|---|---|
| GET `/api/risk/admin/kill-switch` | session + Admin | no body; `{enabled,reason,updated_at,updated_by}` | authoritative `risk_kill_switches` read; 401 unauthenticated, 403 non-Admin |
| PUT `/api/risk/admin/kill-switch` | session + CSRF + Admin | strict `{enabled:boolean,reason?:string}`; returns authoritative state above | global advisory lock, persistent update and append-only audit in one transaction; when newly enabled, transitions/cancels pending entries; broker cancel may occur and is reconciled |
| POST `/api/pnl/trades/{trade_id}/close` | session + CSRF; authenticated owner only | UUID path, no body; `{trade_id,status,message,detail?}` | 404 for non-owned/missing trade; idempotent completion for already closed; 400 for DEMO, attribution/exposure mismatch, unreadable broker, or unsafe protection state; writes durable intent and may cancel protection/place one LIVE close |
| DELETE `/api/auth/admin/users/trade-logs/` | session + CSRF + Admin | strict `{username,scope?:"demo"|"live"|"all"}`; omitted scope remains `demo`; response reports per-mode trades/orders/intents/events/risk/backtest/orphan counts plus zero broker mutations | all scopes require global kill; LIVE/ALL require durable and broker-read gates; atomic deletion + audit; 400 fail-closed, 404 unknown user; no Angel mutation |
| GET `/api/pnl` | unchanged access/filter contract | each trade row now includes nullable `manual_close_status` | DB read only; frontend uses it to disable/show close progress |

Browser WebSockets remain `/api/ws/market` and `/api/ws/strategy`. Angel-facing WebSocket behavior remains read-only: NULL assignment uses the OS default route; explicit assignment binds the deterministic private alias and fails closed with no fallback. Shared feeds reconnect/resubscribe and may drive DEMO fills locally, but never submit an Angel trade.

### 36.3 Current PostgreSQL inventory

The live catalog contains these 37 base tables (including SQLx metadata) and one view:

```text
_sqlx_migrations
alert_delivery_attempts
audit_events
backtest_market_candles
backtest_option_contracts
backtest_runs
backtest_trades
broker_egress_ips
broker_margin_estimates
broker_order_events
broker_position_incidents
broker_reconciliation_blockers
broker_reconciliation_health
broker_secrets
data_repair_backup
email_otps
ichimoku_signal_evaluations
job_runs
manual_trade_close_intents
market_calendar
market_price_ticks
risk_decisions
risk_kill_switches
risk_limits
strategy_events
strategy_execution_intents
strategy_market_snapshots
strategy_orders
strategy_reversal_intents
strategy_scheduler_runs
strategy_signals
trades
user_profiles
user_sessions
user_strategy_activations
user_strategy_configs
users
VIEW broker_deployment_account_safety
```

The two additions after the old 47-migration baseline are:

- `20260904000000_manual_trade_close.sql`: `manual_trade_close_intents`, keyed uniquely by `trade_id`, with user/order FKs, exact requested quantity/side, timestamps, and statuses `requested`, `cancelling_protection`, `submitted`, `partially_filled`, `ambiguous`, `completed`, `failed`, or `reconciliation_required`. Its partial status index drives recovery.
- `20260904010000_broker_deployment_readiness.sql`: adds `broker_credential_revision` to reconciliation health; invalidates legacy health; creates `broker_reconciliation_blockers` (`open|resolved` plus external-active, structurally-unknown, and active-conditional counts); and creates `broker_deployment_account_safety`.

The safety view includes all eight durable LIVE/unresolved classes: open LIVE trades, unsafe closed LIVE trades, nonterminal/ambiguous LIVE orders, LIVE execution intents, LIVE reversal intents, incomplete manual closes, open/operator-required incidents, and open broker-mutation blockers. The prior row locks, global/user advisory locks, `FOR UPDATE SKIP LOCKED` intent claims, unique/partial indexes, client/idempotency keys, monotonic fill watermarks, immutable-audit triggers, order-transition trigger, exit-coverage trigger, and trade safety-terminal trigger remain authoritative. Python must use this schema initially and must not replace DB-enforced correctness with process-local locks.

### 36.4 Verified production feature behavior

#### Global Kill Switch

The authoritative state is the nullable-global row in `risk_kill_switches`. Only an authenticated Admin with CSRF can read or update the global control. The update takes the exclusive global risk advisory lock, row-locks the global value, persists reason/updater/time, and atomically appends an immutable audit event containing previous/requested/resulting state and whether state changed. Repeating the same value is idempotent but still audited.

All non-protective entries check the effective global/user kill state during risk reservation and again at the final locked LIVE submission gate. Enabling the switch moves pending entries toward cancellation; LIVE submitted entries use Angel Cancel and remain `cancelling` until reconciliation. Existing positions are not blindly flattened. SL/target protection, reconciliation, emergency containment, square-off, and user Manual Close remain eligible because they reduce risk. Clear Trades depends on the global switch being enabled and holds a shared global lock, so a concurrent disable cannot commit during cleanup.

#### Clear Trades

Production supports DEMO, LIVE, and ALL. DEMO clears the user's DEMO trade/order graph, mode-attributable intents/events/risk rows, eligible orphan signals/snapshots, and saved backtests; ambiguous unlinked intents are preserved but terminalized, and the demo reset fence advances. LIVE clears only locally recorded LIVE history after safety proof. ALL performs the LIVE proof first and then both graph cleanups in one transaction, so unsafe LIVE state cannot cause a partial DEMO-only clear.

Every scope requires Admin authorization, CSRF, the enabled global switch, global-shared then user-exclusive advisory locks, and an atomic append-only audit. LIVE/ALL require all eight durable counts to be zero, then successful read-only positions, order-book, trade-book, and every conditional/GTT page through that user's credentials and assigned egress, then a second durable check. Nonzero positions, nonterminal/unknown orders, active/unknown conditional rules, malformed payloads, or any read failure block. User identity, profiles, encrypted broker secrets, permissions, egress inventory/assignment, strategy configuration/activation, reconciliation incidents/blockers/health, and prior audits are preserved. The path has a hard zero-broker-mutation contract.

#### `futures_breakout_v3`

The current implementation retains both standard and independently evaluated opening-range gap entries for BUY and SELL. It uses the four-prior-session HH/LL model, actual entry fills for target anchoring, exact contract lot/tick/circuit validation, TP1 lot splitting, SL1 before TP1, SL2 for the runner, monotonic partial-fill accounting, and exact protection slices. A fully processed SL2 (never a touch, pending trigger, submission, or partial fill) atomically closes the source and creates one unique durable reversal intent. BUY reverses to SELL and SELL reverses to BUY for the original configured lots, with a fresh target/SL1/SL2 anchored to the reversal fill and source lineage retained.

LIVE reversal waits for authoritative source-contract flatness, then re-enters through the ordinary permission, kill, readiness, egress, risk, final-gate, stable-tag, and ambiguity pipeline. DEMO reversal uses deterministic local simulation and zero Angel mutations. Stable source-trade uniqueness/session keys plus restart recovery make repeat observations idempotent. The Global Kill Switch blocks the new reversal entry but never disables source-position protection/reconciliation.

#### `supertrend_index_options_v1`

Signals use the underlying SENSEX/NIFTY completed five-minute candles, Wilder RMA ATR period 7 and factor 2, continuous prior-trading-session state, and a genuine direction flip: down-to-up buys nearest-expiry ATM CE; up-to-down buys nearest-expiry ATM PE. Entries are long options only. Opposite exposure is protected/cancelled/closed before replacement entry. Per-user lots/TP/SL remain separate after one shared signal/contract lookup. SL is STOPLOSS_MARKET, broker acknowledgement of exact coverage precedes LIMIT target submission, and exit fills/cancel races use the shared reconciliation engine.

The actual production entry window is 09:15 through 15:09 Asia/Kolkata. At/after **15:10 Asia/Kolkata**, no new entry is allowed and durable `SQUARE_OFF` intents are created. Protective orders must become broker-terminal before MARKET close; rejected retries use a new deterministic attempt key, while active/ambiguous closes are not duplicated. The scheduler continues minute-keyed retries through the 15:30 watchdog and restart recovery. Futures Breakout's separate 15:20 expiry checkpoint is unchanged.

#### Manual Close Trade and broker-side manual close

The user-facing API accepts no quantity, side, account, credential, or source-IP input. Ownership is enforced in the initial and refreshed trade query. It supports open LIVE trades only; DEMO returns 400. Before intent creation it runs normal LIVE reconciliation, performs fresh order-book and positions reads, requires exactly one attributable open local trade for exchange/token, and requires exact signed broker/local quantity and compatible symbol. A mismatch marks `RECONCILIATION_REQUIRED` and does not guess.

One row per trade and a `manual-close:{trade_id}` advisory key make requests converge. Side is the opposite of the stored direction and quantity is the server-derived remaining exposure. Existing exit protection is cancelled through the normal account-specific egress path and must be broker-terminal before the `EMERGENCY_CLOSE` MARKET order with stable `mc-...` session identity is submitted. Global/user kill switches do not block it. Partial fills reduce durable remaining exposure and keep the intent; ambiguous writes are never blind-retried; definite failure requires a new explicit request; completion waits for broker fill/position reconciliation. Actual fills drive weighted exit price, realized P&L, exit time/reason, and terminal safety state. Intent durability is transactional; the request audit is currently best-effort immediately after that commit and must be characterized in parity tests.

When a user closes directly in Angel, broker flatness alone is insufficient. Rulenix requires successful positions and order-book evidence and, for a local-open/broker-flat candidate, successful trade-book evidence. There must be exactly one attributable local trade and exact opposite-side post-entry fills for the remaining quantity whose order IDs are not Rulenix-owned. The weighted attributable fills set `MANUAL_BROKER_CLOSE` price/P&L. Missing, failed, structurally ambiguous, multi-trade, wrong-side, wrong-quantity, or Rulenix-owned evidence leaves the trade unresolved. Stale protection is cancelled and confirmed terminal before local closure. `BROKER READ FAILURE != BROKER FLAT` remains invariant.

#### Offline account and LIVE readiness

`live_ready` is not equivalent to deployment-safe. Deployment evaluates each account's eight durable counts. A readable, broker-safe account with zero durable state may deploy and become LIVE-ready after full revision-bound reconciliation. An unreadable/offline account with zero durable state may permit the platform deployment but is not LIVE-ready. Any durable unresolved state or any observed broker exposure blocks deployment, whether the account is active or inactive.

Reconnect first writes reconciliation health unhealthy for the current credential revision, then asynchronously reads positions, order book, trade book when attribution requires it, individual order evidence where the operational classifier requires it, and all conditional/GTT pages through the assigned egress. Runtime readiness requires healthy state, exact current credential revision, freshness within five minutes, and no unresolved incidents/blockers. Credential or session revision changes invalidate old health. Read failure/partial evidence preserves blockers and cannot restore LIVE. Successful fresh full reconciliation with no unsafe exposure resolves blockers and restores entry eligibility.

#### Angel egress and synthetic OCO

NULL egress means no application source bind and normal OS routing; it does not hard-code the primary public IP. An explicit assignment must reference a unique `CONFIGURED/VERIFIED` inventory row, maps the public IP to a stable private alias, binds both REST and WebSocket TCP, and fails closed with no default-route fallback. The restricted root helper alone manages host `/32`, container alias, return route, and SNAT, with serialized 0600 Unix-socket access. Startup rehydrates configured inventory before workers. Admin registration, verification, and unique assignment are audited/locked. Both currently configured production IPs are listed in 36.1; Python should consume inventory and never encode either address as a constant.

The strict synthetic OCO classifier provenance gap from section 12 is resolved in this release by `scripts/broker-exposure-classifier.mjs`, its tests, and `production-broker-safety-gate.mjs`. It is an operational deployment-gate classifier, not an Angel mutation or ordinary runtime order-state shortcut. `synthetic` requires the strict empty-status/ID Android `OCO_LIMIT` informational shape, matching position net zero, exact individual-order HTTP 200 broker-false `AB1007`, successful conditional inventory with no match, trade-book with no exact trade, and no executable sibling. Any absent/malformed/failed evidence returns `unknown`; genuine nonterminal state is `active`; known terminal state is `terminal`. UNKNOWN/active blocks. Python must preserve the full conjunction and fixtures, never generalize from symbol, `SE-`, Android marker, or `OCO_LIMIT` alone.

### 36.5 Current workers and concurrency contract

There are 13 logical worker/recovery roles, although several are phases of the single scheduler leader rather than independent services:

| Role | Trigger/cadence and ownership | Locks/idempotency/crash behavior | Angel mutation capability |
|---|---|---|---|
| Scheduler leader | dedicated DB connection; `pg_try_advisory_lock`; retry leadership every 10 s; main tick every 5 s | one active instance; marks abandoned scheduler runs failed and claimed intents retryable on startup | dispatch only |
| Futures scheduler/snapshot/contract refresh | exact IST actions; 15-minute catch-up; transient retry about 30 s; contract checks in five-minute buckets | durable scheduler keys/snapshots and calendar fail-closed | entries/exits through execution pipeline |
| SuperTrend candle/signal worker | exact completed five-minute boundaries 09:15-15:30 IST; signal expires after 90 s | candle identity, continuous history, unique signal/audience transaction | replacement entry and opposite close through intents |
| Execution-intent workers | every scheduler cycle; up to eight concurrent | `FOR UPDATE SKIP LOCKED`, durable claim/status/next attempt, stale-claim recovery, final gates | Place for LIVE entries; DEMO local only |
| Protection recovery | every 5 s | trade safety state, exact coverage trigger, stable child tags, uncertain-write reconciliation | Place/cancel SL, target, emergency close |
| Normal broker reconciliation | every 5 s for exposure/incident audience; per-user concurrent tasks | successful reads required, monotonic fill watermark, transition triggers, durable incidents | may cancel partial remainder/protection; never blind-place ambiguous order |
| Full LIVE-readiness reconciliation | immediately after broker connect/reconnect; retried by normal audience while unsafe | credential-revision CAS, five-minute freshness, blocker/health rows | broker reads and safety cleanup only; any cleanup cancel follows normal guarded path |
| SL2 reversal recovery | every 5 s | unique source trade, intent states/session key, broker-flat proof for LIVE | cancel conflicting entries/exits and Place one reversal entry |
| EOD/expiry square-off | minute-keyed from 15:10 for SuperTrend; 15:20 futures expiry checkpoint; 15:30 watchdog/restart | durable `SQUARE_OFF` intent, terminal-cancel proof, attempt keys | cancel protection and Place MARKET close |
| Manual-close recovery | HTTP trigger plus protection/reconciliation cycles | one intent per trade, manual advisory key, fill watermarks | cancel protection and Place exact MARKET close |
| Shared strategy market feed | demand-driven per exchange/token; heartbeat 10 s, freshness/subscription checks 5 s, exponential reconnect | feed generation/lease guards, persisted tick identity | no Angel trading mutation; may perform local DEMO fills |
| Browser market/strategy WebSockets | per authenticated connection | user-bound source routing; ephemeral strategy broadcast with REST refresh after lag | none |
| Broker/app session maintenance | broker session every 60 s; app-session cleanup hourly; admin jobs HTTP-triggered | per-user mutex + credential revision CAS; DB expiry/revocation; strategy reload resets feeds | no trading mutation directly; refreshed credentials can re-enable later guarded work |

Only one Rust/Python execution authority may run scheduler, intent, reconciliation, protection, reversal, square-off, demo-fill, credential-write, or egress-rehydration behavior. Asyncio locks may bound local work but cannot replace the PostgreSQL ownership/transition mechanisms.

### 36.6 Complete Angel mutation boundary

Angel state changes use only two Rust primitives: `angel::place_order` and `angel::cancel_order`. Modify Order and GTT/OCO create/modify/cancel are not implemented. Trade book, conditional inventory, and individual-order detail in the production gate are reads.

| Semantic path | Caller/authority | Entry/risk/kill/readiness rules | Idempotency, ambiguity, egress, reconciliation |
|---|---|---|---|
| Futures/SuperTrend entry | scheduler-confirmed signal -> durable user intent | active config, mode/permission, limits, market/circuit/margin, kill off, fresh revision-bound LIVE readiness, final locked check | durable order before write, stable tag; ambiguous never resubmitted; per-user egress; order/position reconcile |
| SL1/SL2/target creation | confirmed fill/protection recovery | protective role bypasses exposure-increasing limits and kill but requires exact trade/account/quantity | exact coverage, stable child identity, stop acknowledged before target, reconcile every outcome |
| SuperTrend opposite-position close | confirmed opposite signal | close/protection safety first; replacement entry still passes all normal kill/readiness checks | cancel protection to terminal, one square-off identity, per-user egress/reconcile |
| SL2 reversal entry | fully processed SL2 + durable reversal intent | source closed; LIVE source contract broker-flat; then ordinary entry gates including kill/readiness | unique source/session, two-way, restart recoverable, ambiguous-safe |
| Scheduled EOD/expiry close | durable square-off intent | risk-reducing and kill-safe; exact open trade and terminal protection cancellation | deterministic attempt identities, rejected attempt history, no duplicate active/ambiguous close |
| User Manual Close | authenticated owner + durable manual intent | exact attributable broker/local exposure from fresh reads; kill-safe protective role | server-derived side/quantity, stable manual tag, partial/ambiguous recovery, assigned egress/reconcile |
| Emergency/double-fill containment | protection/reconciliation engine | only exact known residual exposure; never creates discretionary exposure | reconstruct residual trade, one stable emergency close, converge via broker truth |
| Entry cancellation | global/user kill enable, strategy deactivation, session/window shutdown | Admin for kill; owner for activation; automated shutdown from durable policy | local DEMO cancel only; LIVE Cancel remains `cancelling` until broker terminal observation |
| Exit/protection cancellation | manual/EOD/opposite/reversal flow, sibling fill, partial remainder, recovery | exact owned broker order and role; cancellation is risk-control sequencing | no Modify; cancel-and-recreate only after evidence; fills during cancel are delta-accounted |

Every LIVE primitive resolves credentials and egress for the target user. Explicit egress failure is closed, not rerouted. A cancellation acknowledgement does not prove terminality. Every possible-delivery Place outcome is reconciled rather than blindly retried.

### 36.7 DEMO versus LIVE boundary

DEMO and LIVE share strategy calculation and durable local models, but `place_strategy_order_inner` branches on `execution_mode`: LIVE alone loads credentials and invokes `angel::place_order`; DEMO returns a local `DEMO-{uuid}` reference. Every cancel call site checks mode and terminalizes a DEMO row locally before the Angel Cancel branch. DEMO ticks are processed only by `process_demo_tick`. DEMO SL2 reversal is local. Manual Close rejects DEMO. Clear DEMO is local maintenance. Backtesting may use Angel market-data reads but cannot reach an order mutation.

The theoretical shared-code risk surfaces are central place/protection/reversal helpers, kill/deactivation cancellation loops, square-off/emergency helpers, and reconciliation-driven sibling cancellation. Their mode branches and stateful tests are mandatory parity gates. Python shadow mode needs a physical capability boundary in addition to these logical checks: no writer client/secret, no helper socket/network capability, and a read-only production DB role. Required invariant: **DEMO -> ZERO Angel trading mutations**.

### 36.8 Test baseline captured in this reconciliation

| Validation | Result |
|---|---|
| `cargo test` at clean production SHA | PASS: 131 passed, 0 failed, 31 ignored environment-gated tests |
| Stateful isolated PostgreSQL/fake-broker tests, serial, loopback `rulenix_test_*` DB | 29 passed; 2 environment-blocked because the configured local role lacks `CREATEDB`; failures are setup permission only in the two child-database migration/history tests |
| Deployment validation already recorded for the exact release | PASS: all 31 stateful tests, including the two child-database cases, passed in the release environment |
| OCO classifier + offline deployment gate (`node --test`) | PASS: 19 named subtests (4 classifier groups with 13 strict assertions; 15 deployment/readiness tests), 0 failed |
| Frontend Vitest | PASS: 9 files, 29 tests |
| Rust format + Clippy warnings denied | PASS |
| Frontend lint + production Vite build | PASS; 106 modules transformed |

Coverage explicitly exercised includes Global Kill Switch persistence/auth/idempotency/audit, all Clear Trades scopes and atomic rollback, manual close ownership/kill behavior, broker manual-flat attribution/P&L, SL2 reversal both directions and DEMO isolation, partial fills, ambiguous writes, protection recovery, order-transition concurrency, 15:10 DEMO square-off, broker-read failure, all eight offline deployment blocker classes, revision-bound LIVE readiness, OCO evidence conjunction, egress bind/fail-closed behavior, and frontend contracts. No test contacted a real broker trading API or production database. The two locally blocked migration tests are an environment limitation, not a product failure; retain the exact-release deployment result and require a CREATEDB-capable disposable CI role for future reruns.

### 36.9 Rust-to-Python parity matrix

| Subsystem | Rust implementation | Required Python equivalent | Parity test | Safety requirement |
|---|---|---|---|---|
| Authentication | opaque hashed session + CSRF cookies; expiry/password fences | FastAPI dependency/middleware with identical cookies, hashes, renewal and errors | golden auth/CSRF/cookie/expiry responses | no bearer/JWT substitution or weaker cookie policy |
| Authorization | independent Admin/LIVE/backtest flags and ownership queries | explicit dependencies plus owner-scoped SQL | role/ownership matrix for every route | deny by default; Admin does not imply LIVE |
| PostgreSQL | SQLx async, 49 migrations, 37 tables, one view | SQLAlchemy 2 async + asyncpg against unchanged schema; Alembic only for future changes | schema/checksum/catalog and transition golden tests | PostgreSQL remains durable truth; preserve raw locks/triggers |
| Angel REST | source-bound reqwest reads and Place/Cancel primitives | httpx clients split into read capability and narrowly constructed writer | recorded request/response/error/timeout fixtures | no blind retry; redact secrets; exact egress |
| Angel WebSocket | bound `TcpSocket`, auth/subscription/reconnect/staleness | asyncio WebSocket with explicit local address and same framing/timers | packet, reconnect, stale generation, source-IP tests | explicit assignment has no fallback |
| Egress | DB inventory + restricted Linux helper + alias/route/SNAT | retain helper initially; Python invokes same narrow protocol | NULL/default, two explicit IPs, failure, rehydration, concurrency | do not hard-code IP; serialize and fail closed |
| Trading state machine | snapshot -> signal -> intent -> order -> fill -> trade/protection | services/repositories with same durable states and SQL transitions | golden transitions plus crash-point replay | one mutating authority; DB guards final |
| Execution/idempotency | durable row/tag before write; SKIP LOCKED; watermarks | transactional claim worker and stable tags | duplicate worker, lost response, out-of-order fill | possible delivery means reconcile only |
| Risk | locked global/user assessment and reservation | same lock order and projected calculations | boundary/concurrent reservation suite | final pre-Place check; fail closed |
| Global Kill Switch | persistent global/user rows, atomic audit, cancellation | same APIs/locks/audit/cancellation orchestration | race claimed intent vs enable; idempotent update | block new entries, preserve risk reduction |
| Clear Trades | scoped transactional graph cleanup with LIVE read gates | identical request default, locks, graph and response counts | DEMO/LIVE/ALL, rollback, repeats, read failures | global kill required; zero broker mutations |
| Futures Breakout | v3 formulas, gap plans, fill-anchored exits, carry | equivalent domain calculations/scheduler | Rust/Python fixture outputs and restart replay | exact lots/ticks/circuits; no stale entry |
| SuperTrend | completed 5m Wilder RMA flips, shared fanout, 15:10 close | same candle identity/history/contract fanout | prior-session flip, stale candle, 15:10 boundary | no repaint/replay; no entry at/after 15:10 |
| SL2 reversal | unique durable two-way intent after confirmed full SL2 | same source transaction and recovery worker | BUY->SELL, SELL->BUY, partial, duplicate, restart | LIVE flat proof + all normal entry gates |
| Manual Close | owner API + one durable intent + emergency-close pipeline | same no-body contract and derived account/side/quantity | ownership, repeat, partial, ambiguous, kill, restart | broker/local exact match; protection terminal first |
| Broker manual close | positions/order/trade attribution and weighted P&L | identical evidence parser/attribution | flat-only refusal, exact/ambiguous fills, P&L | read failure is not flat; protect until proof |
| OCO classifier | strict operational JS conjunction | port exact classifier or call versioned gate artifact | all current classifier fixtures and malformed/read-fail cases | UNKNOWN/UNSAFE blocks; never simplify conjunction |
| Protection/recovery | safety lifecycle, exact stop coverage, emergency containment | durable recovery services with same attempt identities | each crash boundary, cancel/fill race, double fill | confirmed stop before target; exact residual only |
| Offline/LIVE readiness | health revision/freshness + blockers + deployment view | same reconciler and deployment decision | all eight blocker classes, offline clean, reconnect revision | deployment-safe does not imply LIVE-ready |
| Background workers | DB scheduler leader + bounded Tokio tasks | asyncio tasks with dedicated advisory-lock connection | multi-process ownership, stale claims, cancellation/restart | Rust and Python workers never overlap as writers |
| Frontend HTTP/WS | 50 HTTP + 2 WS cookie contracts | same routes/methods/schema/status/errors | captured frontend tests + HTTP/WS contract fixtures | React initially unchanged |

### 36.10 Python target and Phase 1A gate

The target remains Python 3.12+, FastAPI, Pydantic v2, SQLAlchemy 2.x async, asyncpg, Alembic, httpx, asyncio, async WebSockets, and pytest. PostgreSQL remains authoritative and React remains unchanged. Redis is not justified by current production: durable coordination, queues, ownership, replay, and locks already live in PostgreSQL. Retaining the restricted Rust/Linux egress helper for the first Python release is safer than rewriting privileged networking during behavioral migration.

Phase 1A pass evidence is complete: exact running commit directly verified; clean reproducible Git source exists; live schema baseline directly queried; the five-commit production delta and recent features are specified; all 50 HTTP and two WS contracts are accounted for; all semantic broker mutation paths and DEMO guards are inventoried; workers/concurrency/recovery are current; and a safe test baseline is recorded. Residual non-blocking risks are the dirty/untracked documentation worktree, production release branch not yet shown on `origin/master`, the local role's missing `CREATEDB` for two repeat tests, the known egress reassignment/open-WebSocket race, and the manual-close audit occurring best-effort after intent commit. These require controls/tests in later authorized phases but do not make current production source unreconstructable or leave functionality absent from this specification.

```text
CURRENT PHASE: 1A
CURRENT_AUTHORITATIVE_RUST_PRODUCTION_COMMIT: 3f788f2a842ef9b1b66366d439431867850e3753
PRODUCTION_SCHEMA: 49 successful / 0 failed / latest 20260904010000
CURRENT PYTHON MIGRATION COMMIT: NONE - PYTHON NOT STARTED
PRODUCTION MODIFIED: NO
LIVE BROKER MUTATIONS: ZERO
PHASE 1A GATE: PASS
```

## 37. Phase 2 — Python foundation implementation (2026-09-07)

Phase 2 was implemented on the isolated `phase2-python-foundation` worktree, based directly on
authoritative Rust commit `3f788f2a842ef9b1b66366d439431867850e3753`. The Rust source, production
deployment, proxy, egress helper, and Angel configuration were not modified.

### 37.1 Architecture and dependencies

`python-backend/` is a Python 3.12+ package using FastAPI, Pydantic v2, SQLAlchemy 2.x async,
asyncpg, Alembic, argon2-cffi, cryptography, asyncio-compatible ASGI, httpx test support, and
pytest/pytest-asyncio. There is no Redis. Settings are environment-driven; production/staging
requires a database URL, HTTPS origin, and a strong OTP HMAC key. Setting
`PYTHON_LIVE_TRADING_ENABLED=true` is rejected at configuration validation.

The application has structured error envelopes (`detail`, `retry_after`, optional `code`), request
correlation via `X-Request-ID`, CORS constrained to the configured origin, and no secret values in
responses. SQL uses the existing PostgreSQL tables directly; no duplicate ORM schema was created.

### 37.2 Database and Alembic adoption

The existing PostgreSQL ledger remains authoritative: 49 successful migrations, latest
`20260904010000`, 37 base tables and one safety view. Alembic is present as an adoption-only
environment with no Phase 2 version scripts and no production upgrade path. Future schema changes
require an explicitly reviewed later phase.

### 37.3 Ported foundation contracts

Implemented foundation routes include liveness/readiness, login/access/logout, OTP request/signup
and password-reset verification/reset contracts, current account profile read, Admin user inspection
and safe permission updates, risk state/limits reads and DB-only updates, and the persistent global
kill switch. Session cookies are `rulenix_session` (HttpOnly) and `rulenix_csrf` (readable), with
hashed opaque tokens, idle/absolute expiry, password-change fencing, and CSRF validation on
mutating requests. Admin checks are independent of LIVE permission; ownership remains explicit.

The global kill switch uses a PostgreSQL advisory transaction lock, persists state across restart,
is idempotent, and appends an audit event atomically. It does not perform broker-side cancellation;
the response records `trading_actions_deferred=true`.

### 37.4 Explicitly deferred behavior

Broker connect/profile writes, trading-mode mutation, egress assignment/verification writes, trade
logs/clear-trades, P&L/manual close, strategies/backtesting,
workers, and both WebSocket channels return a controlled `503 python_foundation_deferred` or are not
registered. Signup stores the API key only in the Rust-compatible AES-256-GCM `broker_secrets`
envelope and never in the legacy plaintext column. No Angel client, order lifecycle, or external
broker call is present in Python. Health/readiness explicitly report foundation ready and live trading
unavailable.

### 37.5 Phase 2 parity and test evidence

| Area | Result | Evidence |
|---|---|---|
| Python foundation import/compile | PASS | `python -m compileall -q app` |
| Security validation and OTP HMAC | PASS | `tests/test_security.py` |
| Health and deferred boundary | PASS | `tests/test_api_contract.py` |
| Mutation inventory | PASS | `tests/test_no_mutation.py` |
| Formatting/lint | PASS | `python -m ruff check app tests` |
| Unit/API tests | PASS | `python -m pytest -q` — 3 passed, 1 optional isolated-PostgreSQL test skipped without `TEST_DATABASE_URL` |
| PostgreSQL production schema | NOT TOUCHED | Alembic versions intentionally empty |
| Rust release baseline | PRESERVED | baseline `3f788f2…`; no Rust files changed |

The local PostgreSQL role's pre-existing `CREATEDB` limitation is not weakened or bypassed. A
disposable isolated integration database remains a Phase 3 prerequisite for full auth/RBAC and
kill-switch transition fixtures.

### 37.6 Phase 3 prerequisites

Before any broker layer work: add isolated PostgreSQL integration fixtures and golden cookie/error
parity tests; complete signup credential encryption and SMTP delivery configuration; port remaining
safe account/profile contracts; review frontend route compatibility; and obtain explicit Phase 3
authorization. No Phase 3 work is included here.

```text
CURRENT PHASE: 2
CURRENT_AUTHORITATIVE_RUST_PRODUCTION_COMMIT: 3f788f2a842ef9b1b66366d439431867850e3753
PYTHON FOUNDATION WORKTREE: .runlogs/python-foundation (branch phase2-python-foundation)
PYTHON ANGEL MUTATION PATHS: 0
PRODUCTION MODIFIED: NO
LIVE ORDERS PLACED: 0
LIVE ORDERS MODIFIED: 0
LIVE ORDERS CANCELLED: 0
PHASE 2 GATE: PASS — FOUNDATION ONLY; BROKER LAYER DEFERRED
```
