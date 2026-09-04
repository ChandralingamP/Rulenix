# Production broker deployment safety

The production gate treats broker readability and broker flatness as separate facts. A failed positions, order-book, trade-book, conditional-rule, order-detail, credential, or configured-egress check is **unreadable**, never flat.

For every Rulenix user, the gate snapshots these durable LIVE-risk classes:

- open LIVE trades;
- closed LIVE trades whose safety lifecycle is not `CLOSED` or whose recorded broker net quantity is nonzero;
- nonterminal or ambiguously acknowledged LIVE strategy orders;
- pending, claimed, retrying, or submitted execution intents attributable to LIVE mode, a LIVE order, or a LIVE trade;
- pending, processing, waiting, submitted, or retryable-failed LIVE SL2 reversals;
- every incomplete LIVE manual-close intent;
- open or operator-required broker-position incidents;
- durable broker-reconciliation blockers created by previously observed external active orders, structurally unknown orders, or active conditional rules.

The decision matrix is:

| Broker read | Durable LIVE-risk total | Broker exposure | Deployment | LIVE-ready |
|---|---:|---:|---|---|
| Successful | 0 | none | allowed | allowed after the revision-bound full reconciliation |
| Successful | any | any | blocked | blocked |
| Successful | any | none | blocked by local unresolved state | blocked |
| Failed/unavailable | 0 | unknown | allowed | blocked |
| Failed/unavailable | any | unknown | blocked | blocked |

Deployment proceeds only if every row is allowed. An offline, locally clean user therefore does not stop a platform release, while an offline user with any known or unresolved LIVE risk fails closed.

Runtime LIVE entries require a healthy reconciliation record for the current broker credential revision, checked within five minutes. A new or refreshed Angel session first marks that record unhealthy, then automatically reads positions, order book, trade book, and all conditional-rule pages through the user's configured egress. External exposure creates durable incidents/blockers. Only a completely successful authoritative read without unresolved state restores LIVE entry eligibility. Read failure preserves existing durable blockers and keeps eligibility disabled.

The deployment gate is broker-read-only. It calls no place, modify, or cancel endpoint.
