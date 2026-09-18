export const unresolvedFields = [
  "open_live_trades",
  "unresolved_closed_live_trades",
  "unresolved_live_orders",
  "unresolved_live_execution_intents",
  "unresolved_live_reversals",
  "unresolved_live_manual_closes",
  "unresolved_broker_incidents",
  "unresolved_broker_mutations",
];

export function localUnresolvedTotal(local = {}) {
  return unresolvedFields.reduce((total, field) => total + Math.max(0, Number(local[field]) || 0), 0);
}

export function deploymentAccountDecision({
  brokerReadable,
  brokerSafe,
  brokerExposureObserved = false,
  requiresAuthoritativeBroker = false,
  rulenixOwnedExposure = 0,
  ambiguousExposure = 0,
  local = {},
}) {
  const localUnresolved = localUnresolvedTotal(local);
  if (brokerReadable) {
    const owned = Math.max(0, Number(rulenixOwnedExposure) || 0);
    const ambiguous = Math.max(0, Number(ambiguousExposure) || 0);
    // brokerSafe is retained only for compatibility with older callers/tests.
    // New callers must supply explicit ownership counts.
    const legacyUnsafe = brokerSafe === false
      && rulenixOwnedExposure === 0 && ambiguousExposure === 0;
    const allow = !legacyUnsafe && owned === 0 && ambiguous === 0 && localUnresolved === 0;
    return {
      allow,
      liveReady: allow,
      classification: owned > 0 ? "readable_rulenix_owned_exposure"
        : ambiguous > 0 ? "readable_ambiguous_exposure"
          : legacyUnsafe ? "readable_unclassified_broker_exposure"
            : localUnresolved > 0 ? "readable_local_unresolved_live_state" : "readable_safe",
      localUnresolved,
    };
  }
  if (brokerExposureObserved) {
    return {
      allow: false,
      liveReady: false,
      classification: "unreadable_broker_exposure_observed",
      localUnresolved,
    };
  }
  if (requiresAuthoritativeBroker) {
    return {
      allow: false,
      liveReady: false,
      classification: "unreadable_live_capable_account",
      localUnresolved,
    };
  }
  return {
    allow: localUnresolved === 0,
    liveReady: false,
    classification: localUnresolved === 0
      ? "offline_locally_flat"
      : "offline_with_unresolved_live_state",
    localUnresolved,
  };
}

export function platformDeploymentAllowed(decisions) {
  return decisions.every((decision) => decision.allow);
}
