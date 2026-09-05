# Admin Clear Trades

`DELETE /api/auth/admin/users/trade-logs/` accepts an administrator-only JSON body:

```json
{"username":"TRADER","scope":"demo|live|all"}
```

The scope defaults to `demo` for compatibility. Every scope requires the persistent Global Kill Switch to be enabled. The transaction acquires the shared global risk advisory lock followed by the target user's exclusive advisory lock, matching execution lock order.

Clear Trades is local Rulenix maintenance. It never places, modifies, cancels, or closes an Angel order or position.

- `demo` removes the target user's DEMO trades, simulated orders, linked execution state, DEMO risk/event rows, orphaned signal/snapshot rows, and backtest runs. It advances the existing demo reset fence. LIVE records are preserved.
- `live` removes only terminal, reconciled-safe LIVE history and its linked local orders/intents/events/risk rows. DEMO and backtest records are preserved.
- `all` combines both graph cleanups in one transaction. If LIVE safety fails, neither LIVE nor DEMO is partially cleared.

Before `live` or `all`, all eight durable columns in `broker_deployment_account_safety` must be zero both before and after the broker read. Rulenix then reads Angel positions, order book, trade book, and every conditional/GTT page through the selected user's credentials and egress. Every response must be readable and structurally classifiable; positions must have zero net quantity, every broker order must be terminal, and every conditional rule must be terminal. A read failure or unknown object is not flat and blocks cleanup. Unsafe broker evidence also invalidates LIVE reconciliation health.

User rows, credentials, profile/connection configuration, egress inventory and assignment, permissions, strategy settings/activation, reconciliation incidents/blockers, and immutable audit history are preserved. The successful cleanup audit event is committed atomically with the deletion. Repeating a successful clear is safe and returns zero deleted records.
