# Manual LIVE trade close and reconciliation

`POST /api/pnl/trades/{trade_id}/close` requests a risk-reducing close for the authenticated user's own open LIVE trade. The request has no body: account, contract, side, remaining quantity, broker credentials, and egress assignment are derived server-side.

Before any mutation, Rulenix successfully reads the Angel order and position books, reconciles known fills, requires exactly one open local trade for the broker contract, and requires broker signed quantity to equal local signed quantity. A mismatch is moved to `RECONCILIATION_REQUIRED`; it is never guessed or flattened blindly.

The durable `manual_trade_close_intents` row makes repeated requests converge. Existing target/stop orders are cancelled through the normal Angel mutation path and must become terminal before an `EMERGENCY_CLOSE` market order can be submitted. The `mc-...` session key distinguishes the resulting `MANUAL_RULENIX_CLOSE` reason. The global and per-user kill switches do not block this protective role, while all new exposure continues to be blocked normally.

Submitted, ambiguous, rejected, and partial broker outcomes remain open locally until reconciliation processes authoritative fills. Ambiguous writes are not retried. A definite failed/rejected manual attempt requires a new explicit user request; a known partial fill updates the remaining exposure and retains the durable close intent.

For closes performed directly at Angel One, a successful flat position read alone is insufficient. Reconciliation also requires one attributable open local trade and opposite-side trade-book fills after entry whose exact quantity matches the local remainder and whose order IDs are not Rulenix-owned. The weighted broker fill price produces `MANUAL_BROKER_CLOSE` P&L. Ambiguous attribution, missing trade-book data, and broker read failures leave the trade unresolved. Stale protective orders are cancelled and confirmed terminal before the local trade becomes closed.
