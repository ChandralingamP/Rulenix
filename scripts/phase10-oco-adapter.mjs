import process from "node:process";
import { classifyBrokerOrder } from "./broker-exposure-classifier.mjs";

let input = "";
for await (const chunk of process.stdin) input += chunk;
const fixture = JSON.parse(input);
const context = fixture.context ?? {};
const readsSucceeded = context.positionsReadSucceeded === true
  && context.tradeReadSucceeded === true
  && context.conditionalReadSucceeded === true;
const classification = readsSucceeded
  ? classifyBrokerOrder(fixture.order, context)
  : "unknown";
process.stdout.write(JSON.stringify({ classification }));
