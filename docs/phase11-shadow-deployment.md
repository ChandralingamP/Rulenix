# Phase 11 production shadow deployment design

## Isolation model

The production shadow is a separate executable and image. It does not import or package the
Rulenix API, Angel broker, credential, egress, trading, reconciliation, or worker packages.
Its image contains only `asyncpg`, the shadow observer, and the three pure strategy calculation
modules. It receives no Angel URL, token, API key, credential-encryption key, egress-helper
socket, frontend route, or public port.

The process uses two PostgreSQL identities:

- `rulenix_shadow_reader`: transaction-read-only and granted SELECT only on the exact columns
  required from Rust-persisted signals, snapshots, intents, cached candles, reconciliation
  health, profile credential revision, and the deployment-safety view.
- `rulenix_shadow_writer`: no privilege on `public`; it can insert observations and upsert the
  observer heartbeat only inside `rulenix_shadow`.

The roles are `NOINHERIT`, `NOSUPERUSER`, `NOCREATEDB`, `NOCREATEROLE`, `NOREPLICATION`, and
`NOBYPASSRLS`, with three-connection limits. The process rejects any source or sink URL whose
username is not the expected narrow identity.

The Python container is attached only to `shadow_internal`, an internal Docker network without
external routing. A 32 MiB HAProxy bridge has a fixed TCP backend of `postgres:5432` and joins
both `shadow_internal` and the existing `rulenix_default` network. It cannot proxy arbitrary
destinations. This avoids attaching or recreating PostgreSQL and prevents the Python container
from reaching Angel hosts at the network layer. The missing broker code and missing broker
credentials provide independent additional controls.

## Inputs and output

Rust remains authoritative. The observer polls durable data already produced by Rust:

- Futures Breakout snapshots, signals, and intents;
- SuperTrend cached index candles, signal events, snapshots, and intents;
- EOD square-off signals and intents;
- reconciliation health and the deployment-safety view.

It opens no Angel REST session, market WebSocket, or market-data subscription. Futures levels
are recalculated from the stored four-session highs/lows. SuperTrend transitions are recalculated
from Rust's cached five-minute index candles. EOD eligibility and reconciliation freshness are
calculated independently. Account identifiers are HMAC-SHA256 pseudonyms before persistence.

`rulenix_shadow.observations` stores the timestamp, pseudonymous account reference, strategy,
instrument, input version, Rust decision, Python decision, classification, mismatch reason,
severity, latency, failure classification, source timestamp, and observer release. Rust has no
query or dependency on this schema. Records are idempotent per source and observer release and
are disposable only through a separately authorized owner operation.

## Resources and failure behavior

- Python shadow: 0.25 CPU, 256 MiB RAM, 64 PIDs, read-only root, 16 MiB tmpfs.
- DB bridge: 0.10 CPU, 32 MiB RAM, 32 PIDs, read-only root, 8 MiB tmpfs.
- Source pool: maximum two connections; sink pool: maximum two connections.
- Poll interval: 15 seconds; batch: 100; lookback: 24 hours.
- No work queue or background trading worker exists.
- Five consecutive poll failures terminate Python so its bounded restart policy can retry.
- Python, its DB bridge, and their network are independent of Rust, frontend, Caddy, and
  PostgreSQL lifecycle ownership.

A Python crash, restart loop, source-read failure, sink failure, or unavailable DB bridge can
only make the internal shadow health check fail. It cannot restart, signal, route traffic to,
or write state consumed by Rust.

## Mandatory pre-start gate

Do not run provisioning or start either shadow container until all of these are freshly true:

1. Rust, frontend, PostgreSQL, internal readiness, and public readiness are healthy.
2. The current Rust release is identified.
3. LIVE broker positions and all exposure-capable orders are authoritatively classified.
4. No deployment action can endanger current LIVE exposure.
5. A fresh encrypted backup exists and a disposable restore has passed.
6. The release archive and shadow image are built from the reviewed Phase 11 commit.
7. Production role/schema provisioning is reviewed and runs without a PostgreSQL restart.

Provision the isolated roles and schema using `scripts/phase11-provision-shadow.sql`, supplying
strong generated passwords through psql variables without writing them to source control. Create
three root-readable Docker secret files containing reader URL, writer URL, and a random 32-byte-or-
longer pseudonym key. Both URLs must target `shadow-db-proxy` and use their corresponding identity.

Start only the fixed DB bridge, then execute the one-shot permission proof from the shadow image:

```sh
docker compose -f docker-compose.shadow.yml up -d shadow-db-proxy
docker compose -f docker-compose.shadow.yml run --rm --no-deps \
  --entrypoint python python-shadow -m app.shadow.verify
```

The proof must report all four authoritative operations denied and the shadow write working.
Only then start Python:

```sh
docker compose -f docker-compose.shadow.yml up -d --no-deps python-shadow
```

These commands do not use `docker-compose.prod.yml`, recreate an existing service, change Caddy,
or change frontend routing.

## Stop and removal

Rollback removes only the two shadow containers:

```sh
docker compose -f docker-compose.shadow.yml stop python-shadow shadow-db-proxy
docker compose -f docker-compose.shadow.yml rm -f python-shadow shadow-db-proxy
```

Do not stop Rust, frontend, PostgreSQL, Caddy, or the egress helper. Do not drop the shadow schema
during the immediate rollback test; retaining telemetry makes the operation recoverable and does
not affect Rust. Verify Rust and public readiness before and after removal.
