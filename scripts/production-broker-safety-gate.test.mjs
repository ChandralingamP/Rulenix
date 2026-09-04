import assert from "node:assert/strict";
import test from "node:test";
import {
  deploymentAccountDecision,
  localUnresolvedTotal,
  platformDeploymentAllowed,
} from "./production-broker-safety-gate-lib.mjs";

const flat = {
  open_live_trades: 0,
  unresolved_closed_live_trades: 0,
  unresolved_live_orders: 0,
  unresolved_live_execution_intents: 0,
  unresolved_live_reversals: 0,
  unresolved_live_manual_closes: 0,
  unresolved_broker_incidents: 0,
  unresolved_broker_mutations: 0,
};

test("disconnected locally-flat account permits deployment but remains LIVE-blocked", () => {
  const decision = deploymentAccountDecision({ brokerReadable: false, local: flat });
  assert.equal(decision.allow, true);
  assert.equal(decision.liveReady, false);
  assert.equal(decision.classification, "offline_locally_flat");
});

for (const field of [
  "open_live_trades",
  "unresolved_closed_live_trades",
  "unresolved_live_orders",
  "unresolved_live_execution_intents",
  "unresolved_live_reversals",
  "unresolved_live_manual_closes",
  "unresolved_broker_incidents",
  "unresolved_broker_mutations",
]) {
  test(`disconnected account with ${field} blocks deployment`, () => {
    const local = { ...flat, [field]: 1 };
    const decision = deploymentAccountDecision({ brokerReadable: false, local });
    assert.equal(decision.allow, false);
    assert.equal(decision.liveReady, false);
  });
}

test("broker read failure is never broker-flat or LIVE-ready", () => {
  const decision = deploymentAccountDecision({ brokerReadable: false, brokerSafe: true, local: flat });
  assert.equal(decision.classification, "offline_locally_flat");
  assert.equal(decision.liveReady, false);
});

test("partial reads that observe broker exposure block even when another read fails", () => {
  const decision = deploymentAccountDecision({
    brokerReadable: false,
    brokerExposureObserved: true,
    local: flat,
  });
  assert.equal(decision.allow, false);
  assert.equal(decision.classification, "unreadable_broker_exposure_observed");
});

test("readable broker exposure blocks even when local state is empty", () => {
  assert.equal(deploymentAccountDecision({ brokerReadable: true, brokerSafe: false, local: flat }).allow, false);
});

test("readable broker-flat account still blocks on unresolved local state", () => {
  const decision = deploymentAccountDecision({
    brokerReadable: true,
    brokerSafe: true,
    local: { ...flat, unresolved_live_orders: 1 },
  });
  assert.equal(decision.allow, false);
  assert.equal(decision.liveReady, false);
  assert.equal(decision.classification, "readable_local_unresolved_live_state");
});

test("one offline inactive user does not block a safe platform deployment", () => {
  const decisions = [
    deploymentAccountDecision({ brokerReadable: true, brokerSafe: true, local: flat }),
    deploymentAccountDecision({ brokerReadable: false, local: flat }),
  ];
  assert.equal(platformDeploymentAllowed(decisions), true);
});

test("all durable counts participate in the local total", () => {
  assert.equal(localUnresolvedTotal(Object.fromEntries(Object.keys(flat).map((field) => [field, 1]))), 8);
});
