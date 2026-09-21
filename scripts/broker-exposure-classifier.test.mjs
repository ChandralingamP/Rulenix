import assert from "node:assert/strict";
import test from "node:test";
import {
  classifyBrokerOrder,
  classifyOrderOwnership,
  classifyPositionOwnership,
  conditionalRuleIsActive,
  ownership,
} from "./broker-exposure-classifier.mjs";

const synthetic = {
  status: "", orderstatus: "", orderid: "", exchangeorderid: "", parentorderid: "",
  uniqueorderid: "SE-IIRA142474_example", ordertype: "OCO_LIMIT", variety: "NORMAL",
  producttype: "INTRADAY", strategycode: "N_Spark_Android_126.7.2", quantity: "65",
  filledshares: "0", unfilledshares: "0", price: "0", triggerprice: "0",
  tradingsymbol: "NIFTY01SEP2624100CE", symboltoken: "46989",
};
const syntheticContext = {
  orders: [synthetic],
  positions: [{ tradingsymbol: synthetic.tradingsymbol, symboltoken: "46989", netqty: "0" }],
  trades: [],
  conditionalReadSucceeded: true,
  conditionalRules: [],
  detail: { httpStatus: 200, brokerStatus: false, errorCode: "AB1007" },
};

test("genuine nonterminal orders block", () => {
  assert.equal(classifyBrokerOrder({ status: "open" }), "active");
  assert.equal(classifyBrokerOrder({ orderstatus: "trigger pending" }), "active");
});

test("terminal orders do not block", () => {
  for (const status of ["filled", "cancelled", "rejected", "expired"]) {
    assert.equal(classifyBrokerOrder({ status }), "terminal");
  }
});

test("active conditional rules block", () => {
  assert.equal(conditionalRuleIsActive({ status: "ACTIVE" }), true);
  assert.equal(conditionalRuleIsActive({ status: "CANCELLED" }), false);
});

test("only fully proven non-executable synthetic records are exempt", () => {
  assert.equal(classifyBrokerOrder(synthetic, syntheticContext), "synthetic");
  assert.equal(
    classifyBrokerOrder({ ...synthetic, producttype: "CARRYFORWARD" }, syntheticContext),
    "synthetic",
  );
  assert.equal(classifyBrokerOrder({ ...synthetic, producttype: "DELIVERY" }, syntheticContext), "unknown");
  assert.equal(classifyBrokerOrder({ ...synthetic, strategycode: "" }, syntheticContext), "unknown");
  assert.equal(classifyBrokerOrder(synthetic, { ...syntheticContext, conditionalReadSucceeded: false }), "unknown");
  assert.equal(classifyBrokerOrder(synthetic, { ...syntheticContext, positions: [] }), "unknown");
  assert.equal(classifyBrokerOrder(synthetic, {
    ...syntheticContext,
    orders: [synthetic, { tradingsymbol: synthetic.tradingsymbol, status: "open", orderid: "1" }],
  }), "unknown");
});

const completeOrder = {
  status: "open", orderid: "BROKER-1", exchange: "NFO", symboltoken: "123",
  tradingsymbol: "NIFTY26SEP", transactiontype: "BUY", quantity: "50", ordertag: "",
};

test("durable ID or tag proves Rulenix ownership", () => {
  assert.equal(classifyOrderOwnership(completeOrder, [{ broker_order_id: "BROKER-1" }]).ownership, ownership.rulenix);
  assert.equal(classifyOrderOwnership({ ...completeOrder, orderid: "OTHER", ordertag: "RX0123456789ABCDEF01" },
    [{ client_order_id: "RX0123456789ABCDEF01" }]).ownership, ownership.rulenix);
});

test("complete unmatched broker order is manual but orphan RX tag is ambiguous", () => {
  assert.equal(classifyOrderOwnership(completeOrder, []).ownership, ownership.manual);
  assert.equal(classifyOrderOwnership({ ...completeOrder, ordertag: "RX0123456789ABCDEF01" }, []).ownership,
    ownership.ambiguous);
  assert.equal(classifyOrderOwnership({ ...completeOrder, symboltoken: "" }, []).ownership, ownership.ambiguous);
});

test("position requires exclusive fill attribution", () => {
  const position = { exchange: "NFO", symboltoken: "123", tradingsymbol: "NIFTY26SEP", netqty: "50" };
  const trade = { ...completeOrder, status: "complete", fillsize: "50" };
  assert.equal(classifyPositionOwnership(position, { orders: [completeOrder], trades: [trade] }).ownership,
    ownership.manual);
  assert.equal(classifyPositionOwnership(position, {
    orders: [completeOrder], trades: [trade], knownOrders: [{ broker_order_id: "BROKER-1" }],
  }).ownership, ownership.rulenix);
  assert.equal(classifyPositionOwnership(position, { orders: [], trades: [] }).ownership, ownership.ambiguous);
});

test("deployment may prove a position manual from complete negative Rulenix history", () => {
  const position = { exchange: "MCX", symboltoken: "571307", netqty: "20" };
  const manual = classifyPositionOwnership(position, {
    orders: [], trades: [], openLocalPositions: [], rulenixContractHistory: [],
    allowDurableNegativeProof: true,
  });
  assert.equal(manual.ownership, ownership.manual);
  assert.equal(manual.evidence, "no_rulenix_durable_contract_history");
});

test("any exact-contract Rulenix history keeps an unexplained position ambiguous", () => {
  const position = { exchange: "MCX", symboltoken: "571307", netqty: "20" };
  const ambiguous = classifyPositionOwnership(position, {
    orders: [], trades: [], openLocalPositions: [],
    rulenixContractHistory: [{ exchange_segment: "MCX", contract_token: "571307", evidence_rows: 1 }],
    allowDurableNegativeProof: true,
  });
  assert.equal(ambiguous.ownership, ownership.ambiguous);
});
