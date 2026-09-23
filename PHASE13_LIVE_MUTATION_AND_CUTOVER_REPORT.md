# Phase 13 LIVE mutation implementation and cutover report

## Decision

Phase 13 is blocked before production preflight and authority transfer. Rust production
`267961f96037580b106f152939952a73586b6a4e` remains authoritative. No production service, database,
broker account, order, position, kill switch, route, credential, or egress assignment was changed.

The Python branch now has a database-fenced, durable place/cancel mutation boundary and typed Angel
place/cancel/modify/GTT transport. Those primitives passed internal fake-broker and isolated
PostgreSQL fault testing. They are not sufficient for cutover: the executable Python application
does not start the authoritative strategy scheduler or any account-scoped reconciliation,
protection, reversal, manual-close, or EOD worker; it has no production authority lease-renewal
lifecycle or production deployment/routing unit; and its LIVE manual-close endpoint still queues a
durable intent and returns a deliberate 503 without touching Angel.

Because these are safety-critical executable gaps, the final production safety inventory, backup,
authority transfer, public routing change, and post-cutover observation were not attempted.

## LIVE capability matrix

| Capability | Rust authoritative implementation | Python implementation | Remaining blocker | Required cutover proof |
|---|---|---|---|---|
| Place | Production lifecycle and recovery | Typed one-shot REST mutation plus durable fenced coordinator | Coordinator is not wired to production scheduler/workers | End-to-end fake lifecycle, worker restart, production wiring |
| Cancel | Production lifecycle and recovery | Typed one-shot REST mutation plus durable fenced coordinator | Protection/manual-close cleanup workers do not call it | Full protection and close lifecycle certification |
| Modify | Available where Rust requires it | Typed one-shot REST transport | No durable coordinator path or lifecycle consumer | Durable ambiguity/restart certification if required |
| GTT/conditional | Production broker inventory and ownership handling | Typed create/modify/cancel transport | No durable coordinator or lifecycle integration | Exact ownership, ambiguity, and restart certification |
| Protection | Production OCO/protection workers | Domain/recovery state and generic place/cancel primitives | No authoritative Python protection worker | Partial-fill, rejection, cancel, AB1007, double-fill tests |
| Manual LIVE close | Production durable close worker | Durable request is stored | Endpoint deliberately returns 503; no mutation worker | Exact owned-close and manual-external non-mutation proof |
| SL2 reversal | Production durable reversal worker | Domain/persistence and DEMO parity | No authoritative Python reversal worker | Full fill/reject/restart/fencing lifecycle proof |
| EOD | Production 15:10 scheduler and recovery | Scheduling/domain parity | No authoritative Python square-off worker | EOD concurrency/restart/kill-safe mutation proof |
| Reconciliation | Production polling and durable incident handling | Read models, classifiers, repository, one-tick recovery primitive | No production broker polling worker is started | Fresh read loop, ownership, feed, and incident health proof |
| Egress | Per-account explicit binding or OS default | REST and WebSocket binding abstractions; explicit binding fails closed | Must be exercised by every future production worker | Linux helper plus per-account REST/WebSocket isolation proof |

## Implemented safety boundary

- `live_mutation_authority` stores the single Rust/Python/none holder, epoch, lease owner, and expiry.
- Authority transfer uses an exclusive PostgreSQL transaction advisory lock. Broker mutations hold
  the matching shared transaction lock through the network operation and validate holder, epoch,
  owner, lease, runtime, and user immediately before the write.
- Rust place/cancel calls use the same database fence, preventing a stale Rust worker from writing
  after a completed Python transfer.
- `broker_mutation_attempts` durably relates the local order, operation, stable fingerprint/reference,
  authority proof, network boundary, broker identity, result, and ambiguity state.
- A pre-network crash may be retried explicitly. A timeout, response loss, or post-network database
  failure remains ambiguous and is never blindly replayed. Duplicate workers cannot submit again.
- The deployment-safety view includes unresolved broker mutation attempts.
- Python LIVE remains disabled by absence of production worker wiring and authority acquisition;
  constructing a transport alone does not grant permission to mutate.

## Certification evidence

- Python full suite: 211 passed, 3 skipped. The skips are the Unix-only egress-helper socket test and
  two permission suites that require explicitly provisioned roles.
- Explicit isolated Phase 11/12 role suites: 22 passed after provisioning disposable test roles.
- Focused Phase 13 broker/authority/egress suite: 19 passed, 1 Windows-only Unix-socket skip.
- Ruff: passed for `app` and `tests`.
- Mypy: passed for `app` (one existing unchecked-body note).
- Phase 10 executable audit: all categories pass; 39 HTTP contracts, 2 WebSockets, 8 database-state
  cases, 5 crash/restart cases, 4 EOD replay cases, 12 OCO cases, 22 Futures Breakout fixtures, and
  6 SuperTrend fixtures matched with zero unexplained mismatch.
- Rust: format and Clippy passed; 142 non-ignored tests passed; 39 isolated PostgreSQL tests passed.
- The Rust PostgreSQL suite includes deterministic concurrency/deadlock coverage and the 66-entry
  replay: 66 expected, dispatched, evaluated, and DEMO; zero duplicates and scheduler misses.
- Frontend: clean install, zero npm advisories, lint passed, 32 tests passed, production build passed.
- Clean migration chain: all 52 migrations, including the Phase 13 authority migration, applied to
  the disposable PostgreSQL cluster using a transaction per migration.

## Security review

- `rustls 0.23.43` was production-reachable through reqwest and SQLx and affected by
  RUSTSEC-2026-0285. The Phase 13 branch narrowly updates only the lockfile entry to `0.23.45`; a
  fresh `cargo audit` reports no vulnerabilities.
- RUSTSEC-2023-0071 has no patched `rsa` release. The `rsa 0.9.10` lock entry is not in the enabled
  dependency graph (`cargo tree --edges all -i rsa@0.9.10` returns no path), so the vulnerable
  private-key operation is not production reachable in this build.
- Yanked `chacha20 0.10.1` is retained only under an inactive optional QUIC dependency graph;
  `cargo tree --edges all -i quinn-proto@0.11.16` and `-i chacha20@0.10.1` return no enabled path.
  Cargo audit classifies it as an allowed warning, not a vulnerability.
- Both full and production-only npm audits report zero vulnerabilities.

## Cutover blockers

1. Wire a production Python strategy scheduler with database leadership, catch-up, bounded dispatch,
   health advancement, and worker isolation.
2. Wire fresh account-scoped broker reconciliation reads and ownership classification before any
   mutation, including individual-order and conditional/GTT evidence.
3. Implement and fake-certify complete protection, manual LIVE close, SL2 reversal, EOD, modify/GTT,
   partial-fill, and recovery workers against the durable mutation coordinator.
4. Add bounded Python authority lease acquisition/renewal/loss shutdown and prove that every worker
   drains or fails closed on lease loss and database reconnect.
5. Add a reviewed, reversible production Python image/service, secret/egress wiring, health checks,
   and API/WebSocket routing procedure while retaining fenced Rust rollback.
6. Re-run the complete gate after those changes. Only then obtain fresh authoritative broker state,
   create and restore-proof an encrypted backup, verify production migration checksums, and consider
   authority transfer.

## Production actions

- Pre-cutover broker inventory: not run; implementation gates failed first.
- Backup/restore proof: not run; no deployment was attempted.
- Authority transfer: not performed.
- Public routing: unchanged.
- Unexpected Angel mutations: none generated by tests; production Angel was not contacted.
- Manual broker activity modified: no.
- Rollback performed: no; Rust never lost authority.
