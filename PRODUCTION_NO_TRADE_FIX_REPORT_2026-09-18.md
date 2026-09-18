# Rulenix production no-trade fix report

## Scope and safety

This investigation continued the read-only September 14-18, 2026 production audit and
used the deployed release as the source baseline. Production was queried read-only. No
production file, database row, process, container, configuration, broker session, or
order was changed. No Angel order or mutation API was called. This fix has not been
deployed.

| Item | Result |
|---|---|
| Base production SHA | `3f788f2a842ef9b1b66366d439431867850e3753` |
| `/opt/rulenix/RELEASE_COMMIT` | Re-verified at the full SHA above |
| Main local SHA | `e75681792584c8e0eee61ecf576247e9d8771005` |
| Main worktree | Dirty before this task (37 entries); left untouched |
| Isolated worktree | `C:\Projects\Rulenix-no-trade-fix` |
| Fix branch | `fix/no-trade-2026-09-18` |
| Fix starting HEAD | Exact production SHA; clean before edits |

## Confirmed root cause A - market data / strategy inputs

### Earliest proven production failure boundary

Scheduled strategy work ceased after September 11. The durable
`strategy_scheduler_runs` history has 1,679 rows from July 12 through September 11,
with its last completed Futures entry runs at 09:16:03 IST on September 11, and no
rows after that date. The backend container stayed up from September 5 and did not
restart. Its PostgreSQL advisory-lock connection remained alive and idle, so another
scheduler replica did not take over. Together with absent scheduled snapshots and
SuperTrend artifacts, this establishes a live process whose scheduler task held
leadership but stopped advancing scheduled work, not a container outage.

In the deployed code, `strategy::start` performed long-running work, including
`reconcile_live(&state).await`, inline in the only five-second scheduler loop. Any one
of those awaits that failed to return stopped snapshot preparation, Futures session
dispatch, SuperTrend cycles, and demand-driven feed maintenance together. There was
no scheduler heartbeat, per-stage duration, or task-stall alert. This single-loop
coupling and lack of supervision is the confirmed code-level failure-propagation
defect; reconciliation was a concrete externally dependent stall path in that loop.

The retained evidence does **not** identify the exact internal broker/database await
at which the task stopped: there is no panic, backtrace, span, or per-subcall duration
in retained logs. It would be unsafe to claim that reconciliation, one Angel endpoint,
or an HTTP status was the originating hang. The fix isolates the known inline
reconciliation hazard and adds a leader-only watchdog; it does not pretend to repair
an unproved originating subcall.

### Independent feed subscription defect

`market_ws::ensure_strategy_feed` inserted the selected contract token into
`strategy_feed_tokens` before a durable order existed. Immediately afterward,
`refresh_requested_tokens` replaced that set with tokens queried only from existing
active orders/open trades (plus configured SuperTrend index tokens). A newly selected
pre-order option token could therefore be discarded before the subscription message
was built. This is an exact subscription-construction bug independent of the stopped
scheduler.

The deployed exchange mapping, SmartAPI action (`1`), mode (`1`), correlation ID,
token string encoding, grouped request shape, binary tick decoder, timestamp decoder,
and token router are correct. No evidence showed an expired contract or wrong exchange
as the week-wide cause.

### WebSocket stage findings

| Stage | Finding for Sep 14-18 |
|---|---|
| Worker started | UNKNOWN. No new affected-window startup is evidenced; an earlier independent feed task could have remained alive. |
| Auth data available | Broker logins existed, but week-specific WebSocket authentication was not retained. |
| Egress resolved | Configured/verified production egress existed; no egress failure was proven. |
| TCP bind / connected / authenticated | Not retained for the affected window. |
| Subscribe attempted / acknowledged | Not retained for the affected window. |
| Tokens subscribed | None proven for the window; the deployed pre-order token overwrite could produce an incomplete list. |
| First / last tick | None. `market_price_ticks` has zero rows for all five days. |
| Reconnect attempts | One pre-window disconnect/reconnect warning exists on Sep 10; no affected-window worker evidence exists. |

The first proven affected-window failure boundary is absent scheduled work before any
new worker startup could be demonstrated. Missing WebSocket stage logs are an
observability gap, not evidence that a specific authentication or subscribe request
failed.

## Snapshot root cause and September 14-18 explanation

The alternating snapshot result was not a candle-calendar, holiday, rollover, or
historical-response bug. After the scheduler stopped, the only remaining snapshot
creation path was `refresh_after_broker_connect`, called by a user login:

- before 08:30 IST it calls only `ensure_contract_metadata`, leaving a row without
  daily levels;
- at or after 08:30 IST it calls `create_snapshot`, fetches completed daily candles,
  and computes levels when four valid prior sessions exist.

The production rows and login audit times match this branch exactly:

| Date | Login/snapshot time IST | Path | Candles | Result |
|---|---:|---|---:|---|
| Sep 14 | login 07:52:16; fetch 07:52:52 | Before 08:30, metadata only | 0 | Five `missing`; daily levels pending |
| Sep 15 | login 00:50:55/00:51:23; fetch 00:51:30 | Before 08:30, metadata only | 0 | Five `missing`; daily levels pending |
| Sep 16 | login 09:05:11; fetch 09:05:24-26 | After 08:30, full snapshot | 4 (Sep 10, 11, 14, 15) | Five `ready` |
| Sep 17 | login 08:18:06; fetch 08:18:26 | Before 08:30, metadata only | 0 | Five `missing`; daily levels pending |
| Sep 18 | login 08:41:06; fetch 08:41:13-15 | After 08:30, full snapshot | 4 (Sep 14, 15, 16, 17) | Five `ready` |

Sep 16 and Sep 18 worked because their login-triggered refreshes occurred after the
08:30 branch threshold. The returned candle dates also prove that prior-session and
holiday/weekend handling worked. Those two successes did not revive the scheduler, so
they produced no scheduler run, signal, or intent.

## Zero-tick explanation

The zero-tick week and alternating Futures snapshots are related by the stopped
scheduler but are not the same data mechanism. Snapshots use Angel historical REST;
strategy ticks use the shared WebSocket.

1. Scheduled strategy work stopped advancing after Sep 11.
2. No SuperTrend cycle requested or maintained its index feed through the normal
   scheduler path during Sep 14-18.
3. The demand-driven WebSocket had no proven affected-window startup or healthy-tick
   evidence; a pre-existing independent task cannot be excluded by retained logs.
4. Even when a pre-order feed was requested, the separate set-replacement bug could
   remove its selected token before subscription.
5. Production persisted zero ticks and therefore had no live tick input to route.

Classification: **two confirmed code defects, with only one confirmed weekly failure
boundary**. Scheduler non-advancement explains missing scheduled snapshots and absent
scheduled feed maintenance. The pre-order token overwrite independently explains
incomplete subscriptions, but retained telemetry cannot prove it was the sole cause
of every zero-tick day.

## Strategy impact

### Futures Breakout V3

The first weekly failure was that `strategy::start` no longer reached scheduled
session work. On Sep 14, 15, and 17, `run_entries` would also have failed closed at
`snapshot.status != "ready"` had it been invoked. On Sep 16 and 18 the snapshot was
ready, but no scheduler run invoked evaluation. Thus no gap plan, signal, or execution
intent was created.

The directional formulas were not changed. Existing passing tests prove:

- gap up only when `Open > HH4`;
- gap down only when `Open < LL4`;
- inside/equal HH4-LL4 does not use previous close to invent direction.

### SuperTrend Index Options V1

The scheduler did not dispatch its five-minute `run_supertrend_cycle`. Consequently
`process_supertrend_instrument` never requested the index feed, assembled its current
candle series, or evaluated a crossover. The zero-signal result precedes option
selection and order intent creation. The forced-exit code remains 15:10 Asia/Kolkata
and was not changed.

## Confirmed root cause B - reconciliation

The current LIVE block is a real local-versus-broker exposure mismatch, not a proven
HTTP 403 and not merely stale health:

- one masked LIVE account has a local open `SELL` GOLDTEN trade, quantity 20;
- current broker net quantity is 0;
- the entry order recorded a fill of 20;
- local target/stop rows are cancelled;
- no attributable broker close order/fill exists locally;
- current safety state is `RECONCILIATION_REQUIRED`;
- the current incident is `LOCAL_POSITION_BROKER_FLAT`.

Without authoritative close evidence the application cannot safely infer a close
price or mutate the local trade to flat. The reconciliation and LIVE-readiness gates
correctly fail closed. An operator must reconcile this trade against authoritative
broker history before LIVE can become ready; this task intentionally does not alter
it.

A secondary bookkeeping defect left the older mutually exclusive
`QUANTITY_OR_DIRECTION_MISMATCH` incident open after the observed state changed to
`LOCAL_POSITION_BROKER_FLAT`. `record_position_incident` reopened/upserted the current
type but only resolved aggregate mismatch incidents after a fully healthy match. The
fix resolves superseded aggregate mismatch classifications for the same
user/exchange/token while keeping the current mismatch open. It does not weaken the
gate or auto-reconcile exposure.

No first failed broker read was found because the current unhealthy result came from
a successful positions read that returned broker quantity zero. Order/trade history
was insufficient to attribute a safe local close. HTTP 403 is not claimed.

## Causal chains

### Market-data chain

`scheduler leader stopped at an unobserved inline await; no isolation/heartbeat`

-> scheduler stopped advancing after Sep 11

-> no scheduled snapshot retries or SuperTrend/feed maintenance

-> Futures levels depended only on login timing; no affected-window WebSocket ticks

-> strategy evaluation absent or snapshot-fail-closed

-> zero signals

-> zero intents/orders/trades

### Reconciliation chain

`broker flat (0) but local open short (-20), with no attributable close fill`

-> active `LOCAL_POSITION_BROKER_FLAT` incident

-> reconciliation health false and trade `RECONCILIATION_REQUIRED`

-> LIVE readiness false

-> LIVE order placement prohibited independently of market data

## Fix

### `backend/src/market_ws.rs`

- Preserve explicitly requested pre-order tokens when durable order/trade tokens are
  refreshed; clear ephemeral requests only when the feed session actually ends.
- Fail and reconnect loudly on an empty subscription instead of silently succeeding.
- Emit distinct rate-limited operational codes for empty subscription, stale/no-tick,
  and disconnect states.
- Log only subscription counts and the first received tick, not per-tick noise or
  secrets.
- Add focused tests for pre-order token retention and actionable failure codes.

### `backend/src/strategy.rs`

- Run full LIVE reconciliation in a single-flight background task, preventing a
  non-returning broker reconciliation from freezing the sole scheduler loop while
  preventing overlapping reconciliations.
- Add a scheduler-leader heartbeat watchdog that emits one transition alert after 60
  seconds without scheduler progress.
- Alert when snapshot creation returns a persisted non-ready result, not only when it
  throws an error.
- Resolve obsolete sibling aggregate mismatch classifications before upserting the
  current incident; preserve the active current mismatch and LIVE block.
- Add focused single-flight and incident-classification tests.

No strategy formula, entry/exit time, database schema, dependency, frontend behavior,
broker mutation path, credential code, or deployment file changed.

## Observability behavior

The fix adds actionable, five-minute-deduplicated operational events through the
existing alert mechanism:

- `market_data_subscription_empty`;
- `market_data_no_ticks`;
- `market_feed_disconnected` (existing code, retained as the fallback);
- `futures_snapshot_missing`;
- `strategy_scheduler_stalled`.

Existing reconciliation incident and LIVE-readiness alerts remain in force. The
watchdog starts only after this process owns the scheduler advisory lock, avoiding a
false critical alert from an expected standby process.

## Tests

Final local results from the isolated production-base worktree:

| Check | Result |
|---|---|
| `cargo fmt --manifest-path backend/Cargo.toml -- --check` | PASS |
| `cargo clippy --manifest-path backend/Cargo.toml --all-targets --locked -- -D warnings` | PASS |
| `cargo test --manifest-path backend/Cargo.toml --locked` | PASS: 135 passed, 0 failed, 31 ignored |
| Market-data focused tests | PASS: 6, including subscription shape, decode/timestamp, token retention, empty/stale diagnostics |
| Frontend lint | PASS |
| Frontend tests | PASS: 9 files, 29 tests |
| Frontend production build | PASS |
| Migration filename validation | PASS |

The 31 ignored Rust tests explicitly require an isolated loopback PostgreSQL database
named `rulenix_test_*`; neither `TEST_DATABASE_URL` nor `DATABASE_URL` was configured,
so no database-backed integration test was run against an unsafe or unknown database.
Existing ignored coverage includes the broker position matrix, reconciliation
recovery/LIVE readiness, and the 15:10 demo square-off integration case. Pure tests
for the exact broker-flat/local-open classification and fail-closed policy passed.

Existing passing regression coverage also includes subscription construction, packet
decode, duplicate/out-of-order tick rejection, Asia/Kolkata connection boundaries,
Monday/weekend previous-session selection, contract rollover selection, valid
SuperTrend signal evaluation, Futures gap boundaries, and previous-close exclusion.

Independent dependency audits failed on lockfile issues already present at the
production base and untouched by this patch:

- `cargo audit`: `rustls 0.23.43`, RUSTSEC-2026-0285 (medium; fixed in >=0.23.45),
  plus a yanked `chacha20 0.10.1` warning;
- `npm audit --audit-level=high`: 7 findings (1 low, 4 moderate, 2 high), including
  high findings in `browserslist` and `js-yaml`.

No dependency update was mixed into this trading reliability fix.

## Remaining risks

1. The originating awaited operation that stalled the Sep 11 scheduler is not present
   in retained telemetry. The isolation prevents reconciliation from repeating the
   week-wide scheduler outage and the watchdog exposes any other stall, but production
   verification must identify any still-hanging subcall.
2. The current LIVE account remains correctly blocked until an operator reconciles the
   local open trade with authoritative broker order/trade history. This commit cannot
   safely clear it.
3. No affected-window WebSocket subscribe acknowledgement was retained. A staged
   read-only verification must observe connect, subscribe count, first tick, and
   freshness after deployment.
4. Database-backed integration tests were unavailable locally. They must run against
   the explicitly isolated test database before the deployment gate.
5. Existing Rust/npm dependency advisories need a separate dependency change and
   regression cycle.

## Deployment recommendation

Do not deploy directly from this investigation. At a separate deployment gate:

1. run the ignored database-backed integration suite against an isolated
   `rulenix_test_*` PostgreSQL database;
2. review this one-commit diff and build an immutable artifact from the fix SHA;
3. establish an operator-approved resolution for the masked LIVE account's broker-flat
   / local-open trade without fabricating a fill;
4. deploy under normal rollback controls without placing a test LIVE order;
5. verify read-only that the release marker matches, scheduler heartbeat advances,
   snapshots are ready after 08:30, subscription count is nonzero, first tick arrives,
   and reconciliation reports the correct current incident;
6. keep LIVE blocked if the authoritative exposure mismatch remains.

This report and commit are suitable for review, not yet for deployment.
