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
  brokerSafe = false,
  brokerExposureObserved = false,
  local = {},
}) {
  const localUnresolved = localUnresolvedTotal(local);
  if (brokerReadable) {
    const allow = brokerSafe && localUnresolved === 0;
    return {
      allow,
      liveReady: allow,
      classification: !brokerSafe
        ? "readable_broker_exposure"
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
