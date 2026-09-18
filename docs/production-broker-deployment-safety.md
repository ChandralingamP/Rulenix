# Production broker deployment safety

The production gate treats broker readability and exposure ownership as separate facts. A failed positions, order-book, trade-book, conditional-rule, order-detail, credential, or configured-egress check is **unreadable**, never safe.

For every Rulenix user, the gate snapshots these durable LIVE-risk classes:

- open LIVE trades;
- closed LIVE trades whose safety lifecycle is not `CLOSED` or whose recorded broker net quantity is nonzero;
- nonterminal or ambiguously acknowledged LIVE strategy orders;
- pending, claimed, retrying, or submitted execution intents attributable to LIVE mode, a LIVE order, or a LIVE trade;
- pending, processing, waiting, submitted, or retryable-failed LIVE SL2 reversals;
- every incomplete LIVE manual-close intent;
- open or operator-required broker-position incidents;
- durable broker-reconciliation blockers created by ambiguous broker activity.

Fresh broker exposure is classified as `RULENIX_OWNED`, `MANUAL_EXTERNAL`,
or `AMBIGUOUS`. Durable local broker IDs/client tags prove Rulenix ownership.
A complete unmatched broker order without a Rulenix tag is manual. A position
is manual only when its exact net quantity is exclusively explained by such
fills. Missing, mixed, malformed, or unmatched `RX...` evidence is ambiguous.

The decision matrix is:

| Broker read | Durable LIVE-risk total | Ownership | Deployment | LIVE-ready |
|---|---:|---:|---|---|
| Successful | 0 | manual only | allowed | exact-contract collision policy applies |
| Successful | 0 | none | allowed | allowed after revision-bound reconciliation |
| Successful | any | any | blocked by local unresolved state | blocked |
| Successful | 0 | Rulenix orphan or ambiguous | blocked | blocked |
| Failed/unavailable | 0 | unknown | allowed | blocked |
| Failed/unavailable | any | unknown | blocked | blocked |

Deployment proceeds only if every row is allowed. An offline, locally clean user therefore does not stop a platform release, while an offline user with any known or unresolved LIVE risk fails closed.

Runtime LIVE entries require a healthy reconciliation record for the current broker credential revision, checked within five minutes. A new or refreshed Angel session first marks that record unhealthy, then automatically reads positions, order book, trade book, and all conditional-rule pages through the user's configured egress. Proven manual observations do not make the account globally unhealthy, but a fresh manual or ambiguous observation for the exact exchange/token blocks that LIVE entry. DEMO execution does not consult this LIVE collision check. Read failure preserves existing durable blockers and keeps LIVE eligibility disabled.

The deployment gate is broker-read-only. It calls no place, modify, or cancel endpoint.
