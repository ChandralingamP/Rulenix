# Algo Trading Strategy Implementation Audit

Audit date: 2026-08-23 (Asia/Kolkata)  
Mode: strict read-only analysis; this report is the only authorized repository write.  
Evidence labels: **CONFIRMED** = executable code/schema verified; **LIKELY** = strong indirect evidence; **UNABLE TO VERIFY** = evidence unavailable; **NOT FOUND** = repository-wide search found none.

## 1. Executive Summary

**CONFIRMED:** The two executable strategies are `futures_breakout_v3` (Strategy 1) and `supertrend_index_options_v1` (Strategy 2), both implemented primarily in `backend/src/strategy.rs`. The Rust backend starts one database-elected scheduler, persists durable signals and per-user execution intents, reserves risk and idempotency atomically, sends orders to Angel One, polls the broker order book every five seconds, processes incremental fills transactionally, and recreates selected failed protection.

Live trading is **not ready for unattended production**. The most important defect is shared by both strategies: the entry fill and open trade are committed before protection is submitted or acknowledged. A crash or a protective submission failure can therefore leave real exposure without an active stop. SuperTrend submits TP before SL, widening this window. Rejected or ambiguous protective orders are not covered by the narrow retry query. No broker position-book reconciliation exists; reconciliation uses only the order book, so orphan positions, manual broker-side changes, and quantity/average-price position mismatches are not detected.

Duplicate entry submission is substantially protected by durable signal uniqueness, intent uniqueness, order idempotency keys, unique client order IDs, per-user PostgreSQL advisory locks, atomic `pending -> submitting` claims, and “do not blindly retry ambiguous submission.” It is not provably impossible: contract-roll retry uses a suffixed idempotency session after a broker rejection classified as contract-unavailable; a false classification or late fill could create two broker orders. An ambiguous order absent from one order-book response remains indefinitely ambiguous rather than being retried, which avoids duplicates but can strand execution state.

Production architecture is documented for Docker and systemd, with Caddy configured for `rulenix.in`; however the actual host/IP, SSH user/port/key, cloud/VPS provider, and deployed topology are **NOT FOUND**. A local ignored `backend/.env` exists and contains sensitive variable names; values were deliberately not copied. No actual `.env.production` was found.

## 2. Repository / System Architecture

```text
Browser / React frontend
        |
        | HTTPS + REST/WebSocket
        v
Caddy (rulenix.in) or Nginx template
        |
        v
Rust/Axum backend :8080
  |-- PostgreSQL (system state, locks, signals, intents, orders, trades, risk)
  |-- Angel One REST (login, quotes, candles, orders, order book, cancellation)
  |-- Angel One WebSocket (shared market ticks)
  |-- SMTP/webhook alerts
  `-- runtime user log files
```

Important components:

| File / component | Purpose and call path | State / external effects |
|---|---|---|
| `backend/src/main.rs:68 main` | Loads `.env`, validates config, connects DB, runs migrations, builds HTTP routes, starts strategy scheduler | PostgreSQL migrations; starts server/tasks |
| `backend/src/strategy.rs:5149 start` | Five-second leader scheduler and recovery loop | Scheduler runs, signals/intents/orders/trades/events; broker calls |
| `backend/src/strategy.rs:3646 run_entries` | Strategy 1 snapshot, gap plan, durable entry signal fan-out | Snapshots, signals, intents |
| `backend/src/strategy.rs:4450 process_supertrend_instrument` | Strategy 2 candles, indicator, fresh reversal, durable fan-out | Candle cache, snapshots, signals, intents |
| `backend/src/strategy.rs:3104 place_strategy_order` | Common risk reservation, idempotency, live/demo submission | Risk decisions, orders, broker events/API |
| `backend/src/strategy.rs:5660 reconcile_live` | Recovers interrupted states and polls per-user order books | Orders, fills, trades, alerts |
| `backend/src/risk.rs:145 assess_and_reserve` | Atomic risk decision plus pending-order reservation | Advisory locks, risk decisions, strategy orders |
| `backend/src/angel.rs:629 place_order` | Angel One order API; classifies rejection/retryable/ambiguous | External broker order |
| `backend/src/market_ws.rs:270 ensure_strategy_feed` | One shared reconnecting Angel feed | In-memory feed lease; ticks/candles/DB prices |
| `backend/src/credentials.rs:163 CredentialStore` | Encrypted broker secrets in PostgreSQL | `broker_secrets`; AES-GCM keys from env |
| `backend/migrations/*.sql` | Actual state constraints and uniqueness | PostgreSQL schema |

No Redis, message queue, Kubernetes manifest, broker callback/webhook, or distributed cache was found.

## 3. Trading Execution Architecture

Actual common path:

```text
5-second elected scheduler
 -> strategy-specific market/signal evaluation
 -> unique strategy_signals row
 -> unique strategy_execution_intents per signal/user/role
 -> bounded concurrent intent claim (FOR UPDATE SKIP LOCKED)
 -> recheck user/config/activation/exposure
 -> place_strategy_order
 -> refresh quote + broker order-book health (live entries)
 -> atomic risk assessment and idempotent pending order reservation
 -> atomic pending->submitting claim
 -> Angel placeOrder (or immediate demo submission/fill)
 -> submitted / failed / rejected / ambiguous
 -> 5-second order-book reconciliation
 -> incremental fill claim and DB transaction
 -> trade opens
 -> protection submitted sequentially
 -> fills cancel sibling exits and update/close trade
```

PostgreSQL advisory lock `rulenix:strategy_scheduler` elects one scheduler replica. User-scoped risk locks serialize reservations. `strategy_signals`, `strategy_execution_intents`, `strategy_orders`, `broker_order_events`, `risk_decisions`, `trades`, `strategy_reversal_intents`, and scheduler/square-off rows are the durable state.

## 4. Strategy 1 — Complete Implementation

**Identity/instruments:** `futures_breakout_v3`; `GOLDTEN`, `GOLDM`, `SILVERM`, `SILVERMIC`, `NATGASMINI` (`instruments.rs:1`). Enabled only when user, strategy activation, instrument config, requested session, and account mode/permission are eligible (`strategy.rs:3653-3664`).

**Market data and levels:** `create_snapshot` obtains the selected MCX futures contract and four prior daily candles. `calculate` requires exactly four highs/lows. It calculates `HH2=max(last two highs)`, `LL2=min(last two lows)`, `HH4=max(four highs)`, `LL4=min(four lows)`. Standard entries are `HH4*1.0012` and `LL4*0.9988`. Targets are ±1.5% of entry. BUY SL1/SL2 are `LL2*0.9988`/`LL4*0.9988`, each falling back to entry−1.5% if not below entry; SELL mirrors using `HH2*1.0012`/`HH4*1.0012`, falling back to entry+1.5% (`strategy.rs:374-454`).

**Gap plan:** compares current session open with previous close. Gap up selects BUY; gap down SELL; equal selects BOTH. If the open has already jumped the selected standard entry, the strategy waits for an opening range and uses range high+0.12% (BUY) or range low−0.12% (SELL) (`strategy.rs:316-371`, `1502-1779`). The exact opening-range candle interval is produced by `resolve_futures_opening_range_plan`; the plan is persisted in the snapshot. Entries run in configured day/evening sessions through scheduled actions with a 15-minute catch-up window.

**Order:** one or both `STOPLOSS_LIMIT` orders, with limit and trigger equal to planned entry. Quantity is exactly `contract lot_size * configured lots`, checked for overflow and mismatch. No tick-size rounding, broker lot-step query, margin estimate, leverage calculation, or risk-per-stop sizing is performed.

**Fill/exits:** on fill, actual fill anchors target/SL levels. The trade is inserted open transactionally with the entry order marked filled. Other active entries for the instrument are then cancelled. Full-quantity SL1 is submitted, followed by a LIMIT target for `ceil(lots/2)`. Target fill cancels other exits; a demo residual receives SL2 immediately, while live residual protection waits until broker cancellation is terminal and is reconstructed by `recover_residual_protective_orders`. A full SL2 close creates a durable stop-and-reverse intent in the opposite direction for the original lot count. Scheduled carry orders refresh levels. No user-facing manual-exit endpoint was found; “manual” broker activity is not reconciled as a position.

**Actual states:** entries can be pending/submitting/ambiguous/submitted/partially_filled/processing/cancelling/filled/rejected/cancelled/failed. Trades are only `open` or `closed` in this path.

## 5. Strategy 1 — Detailed Logic Flow

```text
Leader scheduler -> session calendar open?
 -> scheduled day/evening action due or within 15-minute catch-up?
 -> eligible runners exist?
 -> select current supported futures contract
 -> obtain exactly four prior daily highs/lows and previous close
 -> calculate HH2/LL2/HH4/LL4 and standard entries/exits
 -> quote session open; classify UP/DOWN/FLAT gap
 -> selected entry jumped? yes: wait/resolve opening range; no: standard entry
 -> snapshot gap plan READY?
 -> create unique durable signal + per-user/role intents
 -> intent rechecks open position and other active entry batch
 -> STOPLOSS_LIMIT entry -> common risk/submission path
 -> broker reconciliation sees fill delta; cancel unfilled remainder
 -> create/increase trade -> cancel sibling entry
 -> submit full SL1 -> submit half target
 -> target: cancel siblings, reduce/close; live residual waits for terminal cancels then SL2
 -> SL1: close affected exposure
 -> SL2: close and queue opposite MARKET reversal
```

Code trace: `start -> schedule_session -> run_scheduled_action -> run_entries -> materialize_signal_intents -> process_execution_intents -> execute_entry_intent -> place_strategy_order -> angel::place_order -> reconcile_live_user -> complete_order -> complete_claimed_order`.

## 6. Strategy 1 — Pseudocode

```text
every 5 seconds as DB-elected leader:
  recover interrupted scheduler/intents/orders
  for each supported future and day/evening session:
    if calendar/session closed: persist skipped
    if due (or <=15m catch-up):
      snapshot = four prior daily candles + selected contract
      if not exactly four valid levels: fail, no order
      gap = compare market_open to previous_close
      if selected breakout was jumped:
        if opening range unavailable: wait
        entry = range edge adjusted 0.12%
      else entry = HH4+0.12%, LL4-0.12%, or both when flat
      persist one unique signal and intents for eligible users
  for each due intent:
    atomically claim
    reject if expired/open position/other active batch/ineligible
    quantity = lot_size * lots
    reserve idempotent order under user risk lock
    atomically claim submission; send once
  reconcile broker order book:
    process only new cumulative fill delta
    cancel any partially filled remainder
    on first fill, commit open trade
    submit SL1 for all quantity, then target for ceil(lots/2)
    on target, cancel sibling exits; after confirmed cancellation protect residual with SL2
    on SL2 close, persist and attempt opposite-market reversal
```

## 7. Strategy 1 — Edge Cases

| Scenario | Current behavior / risk |
|---|---|
| Missing/insufficient daily candle | Snapshot not ready; no entry. Protected. |
| Duplicate scheduler evaluation | Durable signal/order uniqueness converges. Protected. |
| Out-of-order/duplicate WebSocket tick | Tick price overwrites latest DB price; no sequence dedupe. Index candle OHLC can incorporate late duplicate/out-of-order ticks. Possible distortion. |
| Invalid/zero/NaN tick | Ignored by `record_tick`/`record_supertrend_index_tick`. Protected. |
| Stale feed | WebSocket reconnects; entry risk rejects price older than configured age. Protected for new entry. |
| Both breakout sides | Intentionally possible only for FLAT gap; one fill triggers sibling cancellation, but simultaneous fills can create unwanted second exposure. Partially protected. |
| Partial entry fill | Processes delta, requests cancellation, creates/protects filled delta; later deltas can increase trade. Partial-fill cancellation failure leaves remainder live. |
| Open position at late second fill | Live fill is marked filled and “ignored” locally (`7436-7460`) rather than creating/closing the broker exposure. **CRITICAL broker/local mismatch.** |
| Deactivation/kill | Cancels submitted entries; does not close open positions. Protective exits remain. Pending/submitting/ambiguous states are not all selected by deactivation query. |
| Restart during submission | Converts stale submitting to ambiguous and reconciles by broker ID/tag; never blind retries. Safe against duplication, may remain stuck. |
| Entry fill then crash before SL | Open trade persists with no guaranteed broker SL. **CRITICAL.** |
| SL2 reversal | Durable one-per-source intent; clears entries/exits and submits MARKET opposite direction. Risk/execution failure retries, but reversal adds financial exposure by design. |

## 8. Strategy 2 — Complete Implementation

**Identity/instruments:** `supertrend_index_options_v1`; underlyings SENSEX (BSE token `99919000`, options BFO) and NIFTY (NSE token `99926000`, options NFO). It is active from 09:15 through 15:19 IST on an open day and mandatory square-off begins 15:20 (`strategy.rs:37-56, 652-668`).

**Candles/indicator:** five-minute index candles over a 14-day lookup window. REST candles are combined with completed live candles. True range uses current high-low and gaps from prior close. ATR(7) is Wilder RMA seeded with the first seven TR average. SuperTrend factor is 2.0; bands trail as coded in `supertrend_points`. Initial direction is DOWN. Only a transition on the latest completed five-minute candle is eligible: DOWN→UP buys a call; UP→DOWN buys a put. Entry must occur from candle close through 90 seconds later; stale recovered reversals are skipped (`strategy.rs:457-621`).

**Contract/entry:** chooses nearest eligible expiry and ATM option for the signal side, quotes underlying and option, then buys long options only with MARKET order. Per-user target/stop defaults: SENSEX +40/−25 option premium points; NIFTY +25/−15, overridable only with positive config. Target=`actual fill+points`; stop=`max(actual fill−points,0.05)`. Quantity=`option lot_size * configured lots`. Same-side exposure skips entry; opposite pending entries are cancelled and opposite open trades are market-squared-off before entry.

**Exit:** full-quantity LIMIT target is submitted first, then full-quantity STOPLOSS_LIMIT. Either fill cancels active sibling exits. At/after 15:20, durable square-off intent cancels protection, waits for confirmation, quotes option, and submits a MARKET exit. No trailing stop, risk/reward validation, or partial-profit tranche exists.

## 9. Strategy 2 — Detailed Logic Flow

```text
Every five-minute scheduler slot 09:15..15:30
 -> day session open and current time before 15:20?
 -> enabled SENSEX/NIFTY runners?
 -> ensure shared index feed
 -> obtain/cache 14-day five-minute candles
 -> ATR(7) Wilder RMA; SuperTrend(2)
 -> latest completed candle is a direction flip today?
 -> signal age <=90 seconds?
 -> unique session st-{index}-{date}-{HHMM}-{CE|PE}
 -> select nearest-expiry ATM CE/PE and quote premium
 -> recheck freshness
 -> per-user snapshot and unique intent
 -> cancel opposite entry; market-close opposite position; skip same-side exposure
 -> MARKET BUY option
 -> reconcile fill; commit open trade
 -> submit full LIMIT TP, then full STOPLOSS_LIMIT SL
 -> TP/SL fill cancels sibling and closes/reduces trade
 -> >=15:20 cancel protection, await terminal cancellation, MARKET square-off
```

## 10. Strategy 2 — Pseudocode

```text
if open trading day and 09:15 <= now < 15:20:
  for SENSEX and NIFTY concurrently:
    runners = eligible active configs
    candles = historical REST/cache plus safely completed live 5m candles
    points = supertrend(candles, ATR_RMA=7, factor=2)
    signal = flip on exactly latest completed candle
    if no fresh same-day signal: return
    choose nearest-expiry ATM CE for UP flip or PE for DOWN flip
    if selection consumed >90s after close: skip
    persist unique signal/intents
    for each user intent:
      cancel opposite-side pending entry
      market-square-off opposite-side open trade
      if same-side trade/order exists: skip
      submit MARKET BUY
      on reconciled fill: commit open trade
      target=fill+configured_points; stop=max(fill-configured_points,0.05)
      submit LIMIT target
      submit STOPLOSS_LIMIT stop
if now >= 15:20:
  persist one square-off intent per open trade
  cancel exits and wait for broker terminal states
  quote option and submit MARKET SELL for remaining quantity
```

## 11. Strategy 2 — Edge Cases

| Scenario | Current behavior / risk |
|---|---|
| Fewer than seven TR inputs | No SuperTrend point/signal. Protected. |
| Missing/invalid REST candles | Parser drops malformed values; insufficient series produces no signal or error. |
| Duplicate candle | Session merge behavior needs stronger uniqueness proof; live cache is keyed by bucket, REST parse may retain duplicates. **LIKELY** indicator distortion possible. |
| Late/out-of-order tick | Broker timestamp accepted within ±24h; late ticks can alter an in-memory bucket until flushed. Edge-completeness check reduces but does not eliminate distortion. |
| Signal recovered >90s | Explicitly skipped. Protected. |
| Multiple scheduler cycles | Signal/session uniqueness and processed-user query prevent repeat. Protected. |
| Opposite position | Protective orders cancelled/awaited, then MARKET square-off; new entry intent continues only through execution flow. Failure enters retry path. |
| Same-side position/order | Skipped. Protected at query level, but no single atomic position+order exclusion constraint. |
| TP accepted, SL rejected | Position has TP but no stop; retry only for selected error classes. **CRITICAL.** |
| TP rejected before SL call | Function returns before submitting SL. **CRITICAL.** |
| 15:20 quote unavailable | Square-off intent goes `retry_wait`; position stays protected only if prior exits were not already terminal. Code cancels exits before quote, so it may be temporarily unprotected. **CRITICAL/HIGH.** |
| Market close/halt | Broker rejection/retry classification; no exchange halt state. Square-off may fail and retry. |

## 12. Strategy 1 vs Strategy 2

| Feature | Strategy 1 | Strategy 2 |
|---|---|---|
| Signal | Four-day breakout plus gap/opening-range plan | Five-minute SuperTrend reversal |
| Entry | STOPLOSS_LIMIT, BUY/SELL futures | MARKET BUY ATM CE/PE |
| Existing position check | Same user/strategy/instrument | Same option side; opposite is closed |
| Duplicate protection | Signal+intent+order unique keys | Same plus candle-side session and processed-user query |
| Position sizing | Fixed configured lots × contract lot | Fixed configured lots × option lot |
| Risk validation | Common exposure/daily limits | Same common limits |
| SL | Full SL1; residual SL2 after TP; SL2 can reverse | Full fixed premium-point SL |
| TP | ~1.5%; ceil(half lots) | Full fixed premium-point TP |
| Partial fills | Incremental fill/cancel remainder; tranche protection | Same engine, but trade creation behavior is less explicitly tranche-aware |
| Retry | Durable intents; selected protection retries | Same |
| Exit | TP, SL1/SL2, carry, expiry, reversal | TP, SL, reversal square-off, 15:20 square-off |
| Reconciliation | Angel order book only | Angel order book only |
| Restart recovery | Durable scheduler/intents/orders/reversal | Durable intents/orders/square-off |

## 13. Order State Machine

Actual order states observed: `pending`, `submitting`, `ambiguous`, `submitted`, `partially_filled`, `processing`, `cancelling`, `filled`, `rejected`, `cancelled`, `failed`.

```text
pending -> submitting -> submitted -> processing -> filled
                    \-> ambiguous -> submitted/partially_filled/rejected/cancelled/cancelling
submitting -> failed/rejected
submitted <-> partially_filled -> cancelling -> filled/cancelled/rejected
processing -> submitted/partially_filled/filled/cancelled/rejected
failed -> pending (idempotent replacement path)
```

`valid_order_transition` enforces reconciliation transitions, but direct SQL updates elsewhere do not universally call it. Broker status maps complete/completed/filled→filled, rejected→rejected, canceled/cancelled→cancelled; unknown nonterminal status becomes submitted/partially_filled. Each broker reconciliation writes `broker_order_events`. `state_version` is incremented but not used as an optimistic compare-and-swap guard.

Execution-intent states are separate: `pending -> claimed -> submitted/completed/skipped/failed`, with `retry_wait` and `expired`. Signal states: `confirmed`, `dispatching`, `completed`, `partial`, `failed`, `expired`.

## 14. Position State Machine

Actual trade states used by these execution paths are only `open` and `closed`. “Partially open,” “protected,” “closing,” “failed,” and “recovered” are not durable trade states. Protection is inferred from separate order rows. An open trade may therefore be protected, partially protected, awaiting protection, or entirely unprotected with the same `open` status. This prevents a reliable database invariant or operational query from distinguishing safety.

## 15. Duplicate Trade / Idempotency Protection

Classification: **PARTIALLY PROTECTED; duplicate real orders remain POSSIBLE under failure.**

Protections: unique `(strategy_key,instrument,session_key,signal_type)`; unique signal/user/action/role intent; deterministic order idempotency key `(user,snapshot,session,role,trade)`; unique `client_order_id`; atomic risk reservation; user advisory lock; `pending -> submitting` conditional claim; scheduler leader lock; `FOR UPDATE SKIP LOCKED`; no retry of ambiguous submission.

Exact dangerous sequence: broker accepts entry A; connection returns a response/error text classified as contract unavailable rather than ambiguous; local A becomes rejected/failed; contract-roll branch creates a different snapshot/session suffix (`:croll`/`:oroll`) and submits B; A later fills. Because the idempotency key changed, both are valid locally. Also, both BUY and SELL breakout entries can fill before sibling cancellation; the later fill may be marked “ignored” locally while the broker exposure remains.

## 16. Concurrency & Race Conditions

- Scheduler replicas: **PROTECTED** by session advisory lock, provided the DB connection remains alive.
- Same signal workers: **PROTECTED** by unique signal/intent indexes and row claims.
- Risk reservations: **PROTECTED** by global shared and per-user exclusive advisory locks.
- Fill callbacks: no callbacks exist; polling deltas use `processed_quantity` and conditional `processing` claim. **PROTECTED/PARTIAL**.
- Entry vs sibling cancellation: **POSSIBLE** simultaneous fills; cancellation is not atomic at broker.
- TP vs SL: both can fill before cancellation confirmation during a price gap/race. Local delta processing is durable, but over-close/reverse broker exposure is possible. **CRITICAL.**
- Manual exit vs automated exit: manual broker positions are invisible; order-book polling alone cannot establish position quantity. **POSSIBLE.**
- REST vs WebSocket candle: merged paths lack a clearly enforced database uniqueness invariant in the reviewed implementation. **PARTIAL.**
- Shared memory uses Tokio mutexes/generation leases; Rust prevents data races, not semantic late-tick races.

## 17. Risk Management

| Control | Classification | Evidence |
|---|---|---|
| Positive finite lots/quantity/prices | IMPLEMENTED | `risk.rs:55-62` |
| Max lots/quantity/notional/open positions/trades/day | IMPLEMENTED | `risk.rs:64-104`, atomic reservation |
| Daily realized/unrealized loss | IMPLEMENTED | `risk.rs:105-119`, DB P&L query |
| Fresh market price | IMPLEMENTED | DB tick age and validity |
| Snapshot ready/current | IMPLEMENTED | current date and <26-hour snapshot |
| Broker reconciliation health before live entry | IMPLEMENTED/PARTIAL | Order-book reachability, not position agreement |
| Global/user kill switch | IMPLEMENTED | Blocks new non-protective orders; cancels selected entries |
| User active/live permission/session | IMPLEMENTED | Rechecked immediately before submission |
| Required stop values | IMPLEMENTED for Futures, PARTIAL for SuperTrend | Values validated, active broker SL not guaranteed |
| Risk per trade based on stop distance | NOT IMPLEMENTED | Fixed lots only |
| Margin/leverage/available funds | NOT IMPLEMENTED | No margin endpoint/control |
| Tick size/lot-step rounding | NOT IMPLEMENTED | Raw floats and contract lot multiplication |
| Max leverage/exposure by correlated underlying | NOT IMPLEMENTED | Generic notional only |
| Consecutive loss/cooldown/max SL/min SL/R:R | NOT IMPLEMENTED | No code found |

Potential bug: Futures risk exposure sums `total_lots` for open trades even after partial target instead of clearly using `remaining_lots`, which can overstate lot exposure; quantity is reduced, so metrics can disagree.

## 18. Stop Loss / Take Profit Safety

# CRITICAL

Both entry handlers execute this unsafe ordering:

```text
broker entry fill observed
 -> INSERT open trade + mark entry filled + COMMIT
 -> submit protection order(s) through separate transactions/network calls
```

Futures submits SL1 before TARGET; SuperTrend submits TARGET before SL1. Neither waits for broker acknowledgement of a stop before treating the trade as open. If the process crashes after commit, if credentials disappear, DB/API fails, broker rejects price/tick/market state, or TP fails first for SuperTrend, a real position remains without intended stop. The recovery query only retries an existing protective row when status=`failed`, no broker ID, and class is `authentication` or `retryable`; rejected/ambiguous protection and a crash before the protective row exists are not comprehensively reconstructed. `recover_residual_protective_orders` covers only live Futures residuals after an exit fill, not newly opened unprotected trades or SuperTrend.

Recommended behavior: durable `PROTECTION_REQUIRED` state committed with fill; submit stop first; require broker acknowledgement; retry/reconcile by deterministic key; if protection cannot be confirmed within a strict deadline, send idempotent emergency market close and page an operator. TP failure should not block SL submission.

## 19. Broker Integration

Angel One is the only broker. REST functions create/refresh sessions, request quotes/candles/order book, submit and cancel orders. Auth uses per-user encrypted API key/JWT/feed token and broker profile data. `place_order` includes `ordertag=client_order_id`. Transport timeout/disconnect is classified ambiguous and not blindly retried; retryable includes selected 5xx/rate/auth conditions; rejection includes 4xx/business failures. A shared WebSocket uses one healthy user session for all requested market tokens, reconnects with exponential backoff/jitter, detects globally stale feed, and records ticks.

Order prices are formatted/sent without instrument tick-size normalization. STOPLOSS_LIMIT uses equal trigger and limit, which can remain unfilled through a gap. No STOPLOSS_MARKET is used by the strategies.

## 20. Broker Reconciliation

`reconcile_live_user` polls Angel order book, matches by broker order ID then order tag, processes monotonic cumulative fill deltas, requests cancellation after any partial fill, and records broker payload. Broker order state is authoritative for known local orders.

**NOT FOUND:** broker position-book fetch; holdings/net-position comparison; startup orphan-position discovery; local trade-to-broker net quantity/average-entry/SL/TP reconciliation; import of unknown broker orders; manual-trade detection. Therefore `LOCAL STATE != BROKER POSITION` is generally not detected or repaired. An ambiguous local order absent from the latest broker order book remains ambiguous indefinitely with an informational diagnostic. This is conservative for duplication but operationally stuck.

## 21. Database Consistency

Trading data resides in PostgreSQL. Signal+intent creation is one transaction. Risk decision+pending reservation is one transaction. Entry fill+trade creation+entry-order completion is one transaction. Exit fill+trade update+order update+reversal intent is one transaction. Broker submission and DB acknowledgement cannot be atomic; client IDs and reconciliation bridge that boundary.

Key constraints: unique signal identity, unique intent fan-out, unique order idempotency key, unique nonblank client order ID, unique reversal source, one square-off intent per trade/action. Missing constraints: one open trade per user/strategy/instrument; aggregate broker exposure invariant; protected-open-trade invariant; one active exit quantity set per trade; state transition CHECK on order status.

Failure `broker succeeds, DB update fails`: local remains submitting; after 30 seconds it becomes ambiguous and order book/tag may recover it. Failure `trade commit succeeds, protection fails`: no encompassing transaction/recovery invariant; critical exposure remains.

## 22. Crash & Restart Recovery

- A: send entry, crash, broker fills — stale `submitting` becomes `ambiguous`; order book/tag match processes fill. If broker order is absent from the returned book, it remains ambiguous.
- B: broker fills before DB trade update — reconciliation retries the unprocessed cumulative delta; trade insert and order update are transactional.
- C: entry trade committed, crash before SL — no guaranteed startup detector for open trade without active stop. **CRITICAL.**
- D: SL fills, crash before local update — known order reconciliation processes fill delta and closes/reduces trade; if SL was placed manually/unknown, not recovered.
- Claimed execution intents reset to `retry_wait`; running scheduler actions reset to failed/due; pending orders older than 30 seconds become retryable failed; processing orders are restored; reversal and square-off intents retry.

## 23. Error Handling & Retry Logic

Durable entry intents retry up to 12 attempts before expiry. Delay is 5 minutes for rate limit, 15 minutes for auth/session, 5 minutes for market data, otherwise exponential 30 seconds to 30 minutes. Broker submission ambiguity is intentionally not retried. Failed protective orders retry every scheduler cycle only for authentication/retryable classification. WebSocket reconnect uses exponential backoff with jitter. Instrument SuperTrend tasks fail independently.

Significant swallowed/degraded errors: event persistence logs a warning and continues; order-book reconciliation failure marks health unsafe and returns `Ok`, intentionally blocking future live entries; scheduler session-open errors are converted to closed in places; multiple spawned tasks only log failures. `unwrap_or_default` on active token query can silently lose subscriptions for a cycle. No process panic/unwrap was found on the principal live path, but database errors after external broker actions remain unavoidable distributed-transaction hazards.

## 24. Logging & Observability

Available: JSON tracing in staging/production; strategy events include signal/position/order/failure; broker events include local transition, broker/client IDs, diagnostic and bounded broker payload; risk decisions record projected values/health; user log files include contract, action, lots, fill and P&L; operational alerts deduplicate for five minutes. Metrics/readiness endpoints exist.

Gaps: no universal correlation spanning signal ID→intent ID→order→trade in every log line; no durable “trade currently protected at broker” status; no position-reconciliation metrics; missing-order ambiguity can persist without escalation threshold; no audit of unknown broker orders/positions. Credentials implement redacted `Debug` and bounded diagnostics. No direct secret logging was confirmed. The ignored `.env` contains sensitive material and must never be logged or committed.

## 25. Production Environment Map

**LIKELY supported architecture, not proof of current deployment:** Internet → Caddy `rulenix.in,www.rulenix.in` → `127.0.0.1:8080` frontend Nginx → backend → PostgreSQL and Angel One. Docker Compose runs Postgres 16, backend, frontend; backend is read-only with log volume and CA secret. Alternative systemd service runs `/opt/rulenix/backend/rulenix-backend` as `rulenix`, config `/etc/rulenix/backend.env`. Nginx file uses placeholder `app.rulenix.example.com`; Caddy contains the concrete domain.

Health: `/api/health/live`, `/api/health/ready`; Compose polls backend readiness. Migrations run at backend startup behind a DB lock. CI exists in `.github/workflows/ci.yml`; no deployment workflow was found.

## 26. Production Access Configuration

| Item | Result |
|---|---|
| Domain | **CONFIRMED config:** `rulenix.in`, `www.rulenix.in` |
| Production IP/hostname | **NOT FOUND** |
| Cloud/VPS provider | **NOT FOUND** |
| SSH username/port/key/auth | **NOT FOUND** |
| systemd OS user | `rulenix` (not evidence of SSH user) |
| Deployment directory | `/opt/rulenix/backend` in systemd template |
| Environment file | `/etc/rulenix/backend.env` in systemd; `backend/.env.production` in Compose |
| Service name | filename `rulenix-backend.service`; install location **UNABLE TO VERIFY** |
| Docker DB | database/user `rulenix`; password via `secrets/postgres_password.txt` |

No private SSH key or plaintext production credential was inspected or reproduced.

## 27. Environment Variables & Secret Locations

| Variable(s) | Purpose | Required/source/used by | Production / sensitive |
|---|---|---|---|
| `APP_ENV`, `HOST`, `PORT`, `FRONTEND_ORIGIN(S)` | runtime/network/CORS | env; `config.rs`, `main.rs` | APP_ENV/origins required; no |
| `DATABASE_URL` | PostgreSQL credentials/TLS | env file; `config.rs`, `main.rs` | required; **yes, MASKED** |
| `CREDENTIAL_ENCRYPTION_PRIMARY_VERSION`, `CREDENTIAL_ENCRYPTION_KEYS` | encrypt broker secrets | env; `credentials.rs` | required; **yes, MASKED** |
| `OTP_HASH_KEY`, OTP/rate/lockout/session variables | auth security | env/defaults; config/auth/security | key required; **yes** |
| `SMTP_HOST/PORT/USERNAME/PASSWORD/FROM` | OTP/trade/alert email | env; config/alerts | required prod; password **yes** |
| `ANGEL_API_BASE`, `ANGEL_WS_URL` | broker endpoints | defaults/env; angel/market_ws | no secret |
| `CLIENT_PUBLIC_IP`, `CLIENT_LOCAL_IP`, `CLIENT_MAC_ADDRESS` | Angel headers | env; angel | required prod; security-sensitive |
| `FORCE_DEMO_TRADING` | fail-closed live restriction | env; risk/submission | required prod; no |
| `ALERT_WEBHOOK_URL`, `ALERT_EMAIL_TO` | operations alerts | env; alerts | optional; webhook potentially sensitive |
| `INITIAL_ADMIN_USERNAME/EMAIL/PASSWORD` | bootstrap admin | env; `main.rs` | optional all-or-none; password **yes** |
| `RUST_LOG`, `RULENIX_LOG_DIR` | logging | environment/service/compose | no |

Sources found: ignored local `backend/.env` (values not read into report), example templates, Compose env file, systemd environment file. Broker API key, MPIN, TOTP seed, JWT and feed/refresh tokens are stored encrypted in PostgreSQL `broker_secrets`; encryption keys come from environment. No actual production value was verified.

## 28. Dead / Legacy / Duplicate Logic

`supertrend_signal` is explicitly `#[allow(dead_code)]`; active code uses `current_supertrend_signal`. Backtesting contains separate formulas/execution simulation and is not live execution. Documentation and old PDFs/prompts are non-authoritative. Deleted worktree files `backend/src/margin.rs` and `docs/strategy-option-entry-v1.md`, plus migration `20260823000000_remove_margin_and_option_entry.sql`, indicate removed legacy margin/option-entry logic; current `main.rs` has no `margin` module. Old gap-demo migrations only alter historical rows. No alternative live broker implementation was found.

## 29. Critical Issues

### Issue ID: COMMON-CRITICAL-001

Severity: **CRITICAL**  
Location/function: `strategy.rs:7272-7393 complete_supertrend_entry_order`; `7556-7631 complete_claimed_order`  
Current behavior: commits an open trade before protection exists or is broker-acknowledged. SuperTrend submits TP before SL.  
Trigger: crash, DB/network/auth failure, broker rejection, timeout, process termination between steps.  
Danger: unbounded live exposure.  
Expected/recommended: durable protection-required state, SL-first acknowledgement, bounded retries, emergency close and critical alert.  
Files likely requiring changes: strategy/risk/schema/angel/alerts/tests.

### Issue ID: COMMON-CRITICAL-002

Severity: **CRITICAL**  
Location/function: `strategy.rs:6211 retry_failed_protective_orders`  
Current behavior: only failed auth/retryable protective rows with blank broker ID retry; rejected, ambiguous, missing rows and newly open unprotected trades are not comprehensively recovered.  
Trigger: non-retryable classification, crash before row creation, lost acknowledgement.  
Danger: persistent unprotected position.  
Recommended: reconciliation invariant comparing remaining position to acknowledged stop quantity; deterministic repair or emergency close.

### Issue ID: STRAT1-CRITICAL-001

Severity: **CRITICAL**  
Location/function: `strategy.rs:7436-7460 complete_claimed_order`  
Current behavior: a live entry fill arriving while an open trade exists is marked filled and “ignored” locally.  
Trigger: simultaneous opposite/batch fill or late fill after cancellation.  
Danger: real broker exposure has no corresponding trade/protection.  
Recommended: never ignore a real fill; model/offset/reconcile it and immediately protect or close.

### Issue ID: COMMON-CRITICAL-003

Severity: **CRITICAL**  
Location/function: reconciliation architecture (`5998 reconcile_live_user`)  
Current behavior: only broker orders are polled; no broker net-position reconciliation.  
Trigger: manual trade, unknown/late fill, DB loss, broker-side modification.  
Danger: application and broker can disagree indefinitely.  
Recommended: startup/periodic position-book reconciliation with broker authority and orphan quarantine.

### Issue ID: COMMON-CRITICAL-004

Severity: **CRITICAL**  
Location/function: `cancel_active_exits`, exit fill processing  
Current behavior: sibling TP/SL cancellation occurs after a fill; both broker orders can fill during a gap/race.  
Danger: over-close and accidental reversed position.  
Recommended: broker-native OCO/bracket where supported; otherwise position-aware emergency reconciliation and bounded reduce-only semantics.

## 30. High Priority Issues

- `COMMON-HIGH-001`: no tick-size/price-band/lot-step normalization; broker may reject entry or stop (`place_strategy_order`, `angel::order_payload`).
- `COMMON-HIGH-002`: no margin/available-funds/leverage check; fixed lots can exceed actual broker capacity despite generic notional limits.
- `STRAT2-HIGH-001`: 15:20 square-off cancels protection before obtaining quote/submitting close; quote failure can leave exposure unprotected (`4482-4581`).
- `COMMON-HIGH-003`: ambiguous submission absent from latest order book never reaches terminal/alert deadline (`6060-6064`).
- `STRAT1-HIGH-001`: simultaneous BUY/SELL breakout fills are not broker-atomically mutually exclusive.
- `COMMON-HIGH-004`: STOPLOSS_LIMIT with trigger=limit can trigger but remain unfilled in fast/gapping markets.

## 31. Medium Priority Issues

- `COMMON-MEDIUM-001`: trade state cannot express protection/closing/recovery.
- `STRAT1-MEDIUM-001`: open lot exposure appears based on `total_lots`, potentially overstating remaining exposure after TP.
- `COMMON-MEDIUM-002`: no exchange sequence-number deduplication or strict out-of-order tick rejection.
- `COMMON-MEDIUM-003`: session/holiday query failures can be treated as closed/skipped without a strong fail-alert in every caller.
- `COMMON-MEDIUM-004`: direct SQL transitions bypass the central transition validator.

## 32. Low Priority Issues

- `COMMON-LOW-001`: monolithic ~9,000-line `strategy.rs` increases review/regression risk.
- `COMMON-LOW-002`: duplicated runner/config queries and backtest/live formula implementations can drift.
- `COMMON-LOW-003`: some logs lack complete signal→intent→order→trade correlation.
- `COMMON-LOW-004`: placeholder Nginx domain coexists with concrete Caddy domain and can confuse deployment.

## 33. Missing Edge Cases

Additional application-specific cases requiring explicit handling/tests: exchange changes lot/tick size intraday; contract master stale across expiry/holiday; option quote is crossed/zero/illiquid; ATM strike ties; option premium collapses below configured stop points; broker returns cumulative fill lower than prior watermark; broker reuses/truncates order tag; order book pagination omits old active order; shared feed credentials belong to a user whose session is revoked while other users trade; DB leader connection stalls without closing; clock/IST drift around five-minute boundary; DST is irrelevant to IST but host clock skew is not; corporate/broker symbol rename; PostgreSQL failover loses advisory locks; cancellation acknowledged then late fill; TP and SL cumulative fills exceed trade quantity; account mode flips demo/live between intent and protection; kill switch while entry is `submitting`; forced expiry close lacks quote; notification task failure after trade open; migration version mismatch across replicas.

## 34. Required Test Cases

Use this expected-state template for every row: initial DB/broker state; input/event; expected signal/action; expected broker orders/position; expected order/trade rows; expected structured logs/alerts; assertions on uniqueness, quantity, protection and reconciliation.

| Test group / cases | Essential assertions |
|---|---|
| Strategy signals | normal long/call, normal short/put, no signal, duplicate signal/candle, insufficient candle, NaN/zero, stale flip, simultaneous loops | exact direction/formula; one signal and one intent/user/role; no invalid order |
| Eligibility | disabled strategy/instrument, inactive user, demo/live permission, existing position, pending entry, signal immediately after exit, shutdown | no unauthorized submission; durable skip reason |
| Order failures | invalid quantity/tick/lot, margin rejection, market closed, HTTP timeout, DNS/network failure, 429, 4xx, 500, broker unavailable | correct classification; ambiguous never blindly retried; rejected terminal; alert IDs |
| Fill handling | partial fill, multiple deltas, duplicated broker response, cancel/fill race, fill after timeout, response lost | monotonic processed quantity; one trade delta; remainder cancellation; no ignored exposure |
| Protection | entry filled then DB failure; TP failure; SL failure; TP accepted/SL failed; SL accepted/TP failed; stop rejected; crash before/after each submission | every live residual has acknowledged stop or emergency close; current code should expose failing safety tests |
| Exit | TP, SL1, SL2 reversal, SuperTrend SL/TP, 15:20 square-off, manual+automatic, TP/SL race, partial exit, rejected exit | no over-close; correct P&L/reason; sibling terminal; residual protected |
| Recovery | crash before submit, during submit, after broker accept, after fill, before DB trade, after trade/before SL, restart pending/open | deterministic state; no duplicate; broker/local reconciliation |
| Market feed | disconnect/reconnect, stale feed, duplicate/out-of-order packet, corrupt timestamp, missing candle, REST fallback, gap/halt | no stale signal/order; correct candle uniqueness; alert/reconnect |
| Mismatch | broker-only position, DB-only position, quantity/average/SL/TP mismatch, unknown broker order | must detect/quarantine/repair; these tests currently fail because position reconciliation is absent |

Concrete example:

```text
Test: Entry filled but SL submission fails
Initial State: eligible live user; no position/order; fresh price; broker accepts entry
Input/Event: order book reports full entry fill; TP/SL endpoint returns rejection/timeout
Expected Strategy Action: record protection-required; retry safely; emergency-close by deadline
Expected Broker State: either acknowledged SL for full residual or flat position
Expected Database State: never ordinary OPEN without explicit unsafe status
Expected Logs: critical alert with signal/order/trade/broker IDs
Assertions: no unprotected position; no duplicate protection/close
Current result: FAIL — open trade can persist without confirmed SL
```

## 35. Prioritized Remediation Plan

**P0 — before live trading:** protection-required state and SL-first workflow; emergency close; broker position reconciliation; never ignore real fills; prevent/detect double exit and simultaneous breakout fills; normalize tick/lot/price bands; choose stop-market or gap-safe policy; add margin/funds validation. High change risk: execution semantics/schema; require broker sandbox/fault-injection and restart tests.

**P1 — production reliability:** orphan/unknown-order deadlines; full startup reconciliation; atomic per-trade exit coordination; explicit trade protection/closing states; robust partial-fill accounting; square-off quote-before-cancel or atomic replacement; kill/deactivation across all nonterminal states. Require multi-replica and chaos tests.

**P2 — strongly recommended:** split strategy/order/reconciliation modules; unify live/backtest formulas; formal transition repository; persist candle identity/quality; use `state_version` CAS; contract metadata/tick-size service.

**P3 — improvements:** end-to-end correlation IDs, dashboards for unprotected exposure/ambiguity/reconciliation lag, deployment source-of-truth, remove dead code/placeholders, document actual SSH/hosting outside the repository in a secret-safe runbook.

## 36. Files / Functions Reviewed

Repository-wide file/symbol/search passes covered all non-build source files. Deep review included `backend/src/{strategy,risk,angel,market_ws,state,config,main,credentials,instruments,jobs,logs,alerts,notifications,account,ops,backtesting}.rs`; all migrations; environment templates (names and safe examples only); Dockerfiles/Compose; systemd/Nginx/Caddy; GitHub CI; deployment/security/observability/disaster-recovery/strategy documentation; frontend strategy/account/admin surfaces; start/stop/setup scripts. Build output under `backend/target` and binary/image/PDF assets were inventoried but not treated as executable source truth.

## 37. Unable-to-Verify Areas

- Actual production server, IP, SSH configuration, deployed files/processes, environment values, database contents, logs, and broker state: **UNABLE TO VERIFY** without external access, which was neither supplied nor used.
- Broker-specific guarantees for order-tag uniqueness, OCO/reduce-only availability, order-book retention/pagination and exact price-band behavior: **UNABLE TO VERIFY** from repository code.
- Current worktree contains pre-existing modified/deleted/untracked files; conclusions describe that working tree, not necessarily a deployed commit.
- Tests were not executed because the brief allows analysis only and running them could initialize configuration/DB; source tests were inspected.

## 38. Final Assessment

The application has unusually strong local durability and duplicate-submission defenses for an AI-assisted trading system: database leadership, durable fan-out, advisory locks, idempotency keys, client tags, monotonic fill watermarks, broker event audit, and conservative ambiguity handling are all real. Those controls do not solve the most important broker-state invariant. A local `open` trade does not prove a broker stop exists, and no broker position reconciliation proves that local quantity equals real exposure. Accordingly, live trading should remain disabled until P0 items are implemented and fault-injection tests demonstrate that every accepted fill becomes either fully protected or promptly flattened.

## TOP 10 THINGS I SHOULD CHECK FIRST

| Priority | Severity | Problem / strategy / file | Why / potential financial impact |
|---:|---|---|---|
| 1 | CRITICAL | Open trade committed before SL; both; `complete_supertrend_entry_order`, `complete_claimed_order` | Crash/failure leaves unlimited unprotected loss |
| 2 | CRITICAL | Protection retry is incomplete; both; `retry_failed_protective_orders` | Rejected/missing/ambiguous SL can remain absent indefinitely |
| 3 | CRITICAL | No broker position reconciliation; both; `reconcile_live_user` | Orphan/wrong-quantity positions remain invisible |
| 4 | CRITICAL | Real late fill is “ignored” when local trade exists; Futures; `7436-7460` | Untracked and unprotected broker exposure |
| 5 | CRITICAL | TP/SL can both fill; both; exit cancellation/fill handlers | Over-close can create opposite position |
| 6 | HIGH | TP submitted before SL; SuperTrend; `7360-7393` | TP failure prevents any SL attempt |
| 7 | HIGH | Square-off cancels protection before close is assured; SuperTrend; `4482-4581` | End-of-day exposure may be unprotected and fail to exit |
| 8 | HIGH | No tick-size/lot-step/price-band normalization; both; order creation | Broker rejects entry/SL or accepts unintended rounded price |
| 9 | HIGH | Fixed lots without margin/risk-per-stop sizing; both; `risk.rs` | Oversized risk or insufficient-margin failure |
| 10 | HIGH | Simultaneous breakout sides/contract-roll retry race; Futures; `run_entries`, submission retry | Duplicate or opposing real orders under latency/failure |

### Final Safety Review

- [x] No application, strategy, configuration, database, production service, broker order, deployment, commit, or dependency was modified.
- [x] No trades were executed and no services were started/restarted.
- [x] No plaintext password, API secret, token, encryption key, or private key is included.
- [x] The only file created/modified by this audit is `STRATEGY_IMPLEMENTATION_AUDIT.md`.

