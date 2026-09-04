import assert from "node:assert/strict";
import test from "node:test";
import { classifyBrokerOrder, conditionalRuleIsActive } from "./broker-exposure-classifier.mjs";

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
  assert.equal(classifyBrokerOrder({ ...synthetic, strategycode: "" }, syntheticContext), "unknown");
  assert.equal(classifyBrokerOrder(synthetic, { ...syntheticContext, conditionalReadSucceeded: false }), "unknown");
  assert.equal(classifyBrokerOrder(synthetic, { ...syntheticContext, positions: [] }), "unknown");
  assert.equal(classifyBrokerOrder(synthetic, {
    ...syntheticContext,
    orders: [synthetic, { tradingsymbol: synthetic.tradingsymbol, status: "open", orderid: "1" }],
  }), "unknown");
});
