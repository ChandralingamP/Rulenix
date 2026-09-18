import { createDecipheriv } from "node:crypto";
import http from "node:http";
import https from "node:https";
import {
  classifyBrokerOrder,
  classifyConditionalOwnership,
  classifyOrderOwnership,
  classifyPositionOwnership,
  conditionalRuleIsActive,
  orderIdentity,
  ownership,
} from "./broker-exposure-classifier.mjs";
import {
  deploymentAccountDecision,
  platformDeploymentAllowed,
} from "./production-broker-safety-gate-lib.mjs";

const input = await new Promise((resolve, reject) => {
  const chunks = [];
  process.stdin.on("data", (chunk) => chunks.push(chunk));
  process.stdin.on("end", () => resolve(Buffer.concat(chunks).toString("utf8")));
  process.stdin.on("error", reject);
});
const accounts = JSON.parse(input);

const keys = new Map();
for (const entry of String(process.env.CREDENTIAL_ENCRYPTION_KEYS ?? "")
  .split(",").map((value) => value.trim()).filter(Boolean)) {
  const separator = entry.indexOf(":");
  const version = Number.parseInt(entry.slice(0, separator), 10);
  const key = Buffer.from(entry.slice(separator + 1), "base64");
  if (separator <= 0 || !Number.isInteger(version) || key.length !== 32) {
    throw new Error("credential key configuration is invalid");
  }
  keys.set(version, key);
}

function decrypt(userId, kind, value) {
  if (!value) throw new Error(`missing ${kind}`);
  const key = keys.get(value.version);
  if (!key) throw new Error("credential key version is unavailable");
  const nonce = Buffer.from(value.nonce, "base64");
  const encrypted = Buffer.from(value.ciphertext, "base64");
  if (nonce.length !== 12 || encrypted.length <= 16) throw new Error("encrypted credential is invalid");
  const decipher = createDecipheriv("aes-256-gcm", key, nonce);
  decipher.setAAD(Buffer.from(`rulenix:broker-secret:${userId}:${kind}:v${value.version}`));
  decipher.setAuthTag(encrypted.subarray(encrypted.length - 16));
  return Buffer.concat([
    decipher.update(encrypted.subarray(0, encrypted.length - 16)),
    decipher.final(),
  ]).toString("utf8");
}

function bindingAddress(publicAddress) {
  if (!publicAddress) return undefined;
  const octets = publicAddress.split(".").map(Number);
  if (octets.length !== 4 || octets.some((part) => !Number.isInteger(part) || part < 0 || part > 255)) {
    throw new Error("invalid configured egress IPv4 address");
  }
  const raw = (((octets[0] << 24) >>> 0) | (octets[1] << 16) | (octets[2] << 8) | octets[3]) >>> 0;
  const slot = Math.imul((raw ^ (raw >>> 22)) >>> 0, 0x9e3779b1) & 0x003fffff;
  return `100.${64 + ((slot >>> 16) & 0x3f)}.${(slot >>> 8) & 0xff}.${slot & 0xff}`;
}

const base = new URL(String(process.env.ANGEL_API_BASE ?? "https://apiconnect.angelone.in").replace(/\/$/, ""));

function rawRequest(path, apiKey, jwtToken, localAddress, method = "GET", body) {
  const target = new URL(path, base);
  const payload = body === undefined ? undefined : JSON.stringify(body);
  const transport = target.protocol === "http:" ? http : https;
  return new Promise((resolve, reject) => {
    const request = transport.request(target, {
      method,
      localAddress,
      headers: {
        accept: "application/json",
        "content-type": "application/json",
        authorization: `Bearer ${jwtToken}`,
        "x-privatekey": apiKey,
        "x-usertype": "USER",
        "x-sourceid": "WEB",
        "x-clientlocalip": process.env.CLIENT_LOCAL_IP ?? "127.0.0.1",
        "x-clientpublicip": process.env.CLIENT_PUBLIC_IP ?? "127.0.0.1",
        "x-macaddress": process.env.CLIENT_MAC_ADDRESS ?? "00:00:00:00:00:00",
        ...(payload === undefined ? {} : { "content-length": Buffer.byteLength(payload) }),
      },
      timeout: 15_000,
    }, (response) => {
      const chunks = [];
      response.on("data", (chunk) => chunks.push(chunk));
      response.on("end", () => {
        let parsed = null;
        try { parsed = JSON.parse(Buffer.concat(chunks).toString("utf8")); } catch { /* classified below */ }
        resolve({ httpStatus: response.statusCode ?? 0, payload: parsed });
      });
    });
    request.on("timeout", () => request.destroy(new Error("broker read timeout")));
    request.on("error", reject);
    if (payload !== undefined) request.write(payload);
    request.end();
  });
}

async function brokerRequest(path, apiKey, jwtToken, localAddress, method = "GET", body) {
  const response = await rawRequest(path, apiKey, jwtToken, localAddress, method, body);
  if (response.httpStatus < 200 || response.httpStatus >= 300 || response.payload?.status !== true) {
    const code = String(response.payload?.errorcode ?? `http_${response.httpStatus}`).replace(/[^a-zA-Z0-9_.-]/g, "_");
    throw new Error(`broker read failed (${code})`);
  }
  return Array.isArray(response.payload.data) ? response.payload.data : [];
}

async function brokerDetail(uniqueOrderId, apiKey, jwtToken, localAddress) {
  const response = await rawRequest(
    `/rest/secure/angelbroking/order/v1/details/${encodeURIComponent(uniqueOrderId)}`,
    apiKey,
    jwtToken,
    localAddress,
  );
  return {
    httpStatus: response.httpStatus,
    brokerStatus: response.payload?.status,
    errorCode: String(response.payload?.errorcode ?? ""),
  };
}

async function allConditionalRules(apiKey, jwtToken, localAddress) {
  const rules = [];
  for (let page = 1; page <= 100; page += 1) {
    const batch = await brokerRequest(
      "/rest/secure/angelbroking/gtt/v1/ruleList",
      apiKey,
      jwtToken,
      localAddress,
      "POST",
      { status: ["NEW", "CANCELLED", "ACTIVE", "SENTTOEXCHANGE", "FORALL"], page, count: 100 },
    );
    rules.push(...batch);
    if (batch.length < 100) return rules;
  }
  throw new Error("broker read failed (gtt_pagination_limit)");
}

const decisions = [];
const offlineSafeUserIds = [];
let readableAccounts = 0;
let unreadableAccounts = 0;
let openPositions = 0;
let activeOrders = 0;
let unknownOrders = 0;
let syntheticOrders = 0;
let activeConditionalRules = 0;
const exposureTotals = {
  positions: { [ownership.rulenix]: 0, [ownership.manual]: 0, [ownership.ambiguous]: 0 },
  orders: { [ownership.rulenix]: 0, [ownership.manual]: 0, [ownership.ambiguous]: 0 },
  conditionals: { [ownership.rulenix]: 0, [ownership.manual]: 0, [ownership.ambiguous]: 0 },
};

const safeReference = (value) => {
  const raw = String(value ?? "");
  return raw.length <= 4 ? raw : `...${raw.slice(-4)}`;
};

for (let index = 0; index < accounts.length; index += 1) {
  const account = accounts[index];
  let apiKey = "";
  let jwtToken = "";
  let brokerReadable = false;
  let brokerSafe = false;
  let brokerExposureObserved = false;
  let brokerCounts = null;
  let ownershipCounts = { rulenix_owned: 0, manual_external: 0, ambiguous: 0 };
  let evidence = [];
  let diagnostic = "";
  try {
    if (account.egress_ip && (account.egress_configuration_status !== "CONFIGURED"
      || account.egress_verification_status !== "VERIFIED")) {
      throw new Error("configured egress assignment is unavailable");
    }
    const localAddress = bindingAddress(account.egress_ip);
    apiKey = decrypt(account.user_id, "api_key", account.secrets?.api_key);
    jwtToken = decrypt(account.user_id, "jwt_token", account.secrets?.jwt_token);
    const reads = await Promise.allSettled([
      brokerRequest("/rest/secure/angelbroking/order/v1/getOrderBook", apiKey, jwtToken, localAddress),
      brokerRequest("/rest/secure/angelbroking/order/v1/getPosition", apiKey, jwtToken, localAddress),
      brokerRequest("/rest/secure/angelbroking/order/v1/getTradeBook", apiKey, jwtToken, localAddress),
      allConditionalRules(apiKey, jwtToken, localAddress),
    ]);
    const [ordersRead, positionsRead, tradesRead, conditionalRead] = reads;
    const orders = ordersRead.status === "fulfilled" ? ordersRead.value : [];
    const positions = positionsRead.status === "fulfilled" ? positionsRead.value : [];
    const trades = tradesRead.status === "fulfilled" ? tradesRead.value : [];
    const conditionalRules = conditionalRead.status === "fulfilled" ? conditionalRead.value : [];
    const accountOpenPositions = positions.filter((position) => {
      const quantity = Number.parseInt(String(position.netqty ?? position.netQty ?? "0"), 10);
      return !Number.isFinite(quantity) || quantity !== 0;
    }).length;
    let accountActiveOrders = 0;
    let accountUnknownOrders = 0;
    let accountSyntheticOrders = 0;
    let detailsReadable = true;
    for (const order of orders) {
      const identity = orderIdentity(order);
      const missingState = String(order.status ?? order.orderstatus ?? order.orderStatus ?? "").trim() === "";
      let detail;
      if (missingState && identity.uniqueOrderId) {
        try {
          detail = await brokerDetail(identity.uniqueOrderId, apiKey, jwtToken, localAddress);
        } catch (error) {
          detailsReadable = false;
          diagnostic = String(error?.message ?? "order detail read failed");
        }
      }
      const classification = classifyBrokerOrder(order, {
        orders,
        positions,
        trades,
        conditionalRules,
        conditionalReadSucceeded: conditionalRead.status === "fulfilled",
        detail,
      });
      if (classification === "active" || classification === "unknown") {
        const attribution = classification === "unknown"
          ? { ownership: ownership.ambiguous, evidence: "unknown_broker_order_state" }
          : classifyOrderOwnership(order, account.known_orders);
        exposureTotals.orders[attribution.ownership] += 1;
        ownershipCounts[attribution.ownership === ownership.rulenix ? "rulenix_owned"
          : attribution.ownership === ownership.manual ? "manual_external" : "ambiguous"] += 1;
        evidence.push({
          kind: "order", ownership: attribution.ownership, exchange: identity.exchange,
          token: identity.symbolToken, symbol: identity.symbol,
          reference: safeReference(identity.orderId || identity.uniqueOrderId),
          reason: attribution.evidence,
        });
      }
      if (classification === "active") accountActiveOrders += 1;
      if (classification === "unknown") accountUnknownOrders += 1;
      if (classification === "synthetic") accountSyntheticOrders += 1;
    }
    for (const position of positions) {
      const quantity = Number.parseInt(String(position.netqty ?? position.netQty ?? "0"), 10);
      if (Number.isFinite(quantity) && quantity === 0) continue;
      const identity = orderIdentity(position);
      const attribution = classifyPositionOwnership(position, {
        orders, trades, knownOrders: account.known_orders,
        openLocalPositions: account.open_local_positions,
      });
      exposureTotals.positions[attribution.ownership] += 1;
      ownershipCounts[attribution.ownership === ownership.rulenix ? "rulenix_owned"
        : attribution.ownership === ownership.manual ? "manual_external" : "ambiguous"] += 1;
      evidence.push({
        kind: "position", ownership: attribution.ownership, exchange: identity.exchange,
        token: identity.symbolToken, symbol: identity.symbol, quantity,
        reason: attribution.evidence,
        fill_attribution: attribution.details,
      });
    }
    const activeRules = conditionalRules.filter(conditionalRuleIsActive);
    for (const rule of activeRules) {
      const identity = orderIdentity(rule);
      const attribution = classifyConditionalOwnership(rule);
      exposureTotals.conditionals[attribution.ownership] += 1;
      ownershipCounts[attribution.ownership === ownership.rulenix ? "rulenix_owned"
        : attribution.ownership === ownership.manual ? "manual_external" : "ambiguous"] += 1;
      evidence.push({
        kind: "conditional", ownership: attribution.ownership, exchange: identity.exchange,
        token: identity.symbolToken, symbol: identity.symbol, reason: attribution.evidence,
      });
    }
    const accountActiveConditionalRules = activeRules.length;
    brokerCounts = {
      open_positions: accountOpenPositions,
      active_orders: accountActiveOrders,
      unknown_orders: accountUnknownOrders,
      proven_synthetic_orders: accountSyntheticOrders,
      active_conditional_rules: accountActiveConditionalRules,
    };
    brokerReadable = reads.every((read) => read.status === "fulfilled") && detailsReadable;
    const noObservedExposure = accountOpenPositions === 0 && accountActiveOrders === 0
      && accountUnknownOrders === 0 && accountActiveConditionalRules === 0;
    brokerExposureObserved = !noObservedExposure;
    brokerSafe = brokerReadable && ownershipCounts.rulenix_owned === 0
      && ownershipCounts.ambiguous === 0;
    if (brokerReadable) {
      readableAccounts += 1;
    } else {
      unreadableAccounts += 1;
      const failures = reads
        .filter((read) => read.status === "rejected")
        .map((read) => String(read.reason?.message ?? "broker read failed"));
      diagnostic = [diagnostic, ...failures].filter(Boolean).join(";");
    }
    openPositions += accountOpenPositions;
    activeOrders += accountActiveOrders;
    unknownOrders += accountUnknownOrders;
    syntheticOrders += accountSyntheticOrders;
    activeConditionalRules += accountActiveConditionalRules;
  } catch (error) {
    unreadableAccounts += 1;
    diagnostic = String(error?.message ?? "unknown").replace(/[^a-zA-Z0-9_.:()\-]/g, "_");
  } finally {
    apiKey = "";
    jwtToken = "";
  }

  const decision = deploymentAccountDecision({
    brokerReadable,
    brokerSafe,
    brokerExposureObserved,
    requiresAuthoritativeBroker: account.is_active === true && account.can_live_trade === true,
    rulenixOwnedExposure: ownershipCounts.rulenix_owned,
    ambiguousExposure: ownershipCounts.ambiguous,
    local: account.local,
  });
  decisions.push(decision);
  if (!brokerReadable && decision.allow) offlineSafeUserIds.push(account.user_id);
  console.log(`ACCOUNT_${index + 1}=${JSON.stringify({
    user_id: account.user_id,
    username: account.username,
    broker_readable: brokerReadable,
    broker_safe: brokerSafe,
    local_unresolved: decision.localUnresolved,
    deployment_allowed: decision.allow,
    live_ready: decision.liveReady,
    classification: decision.classification,
    broker: brokerCounts,
    ownership: ownershipCounts,
    evidence,
    read_diagnostic: diagnostic,
  })}`);
}

for (const key of keys.values()) key.fill(0);
const allowed = accounts.length > 0 && platformDeploymentAllowed(decisions);
console.log(`BROKER_ACCOUNTS_CHECKED=${accounts.length}`);
console.log(`BROKER_READABLE_ACCOUNTS=${readableAccounts}`);
console.log(`BROKER_UNREADABLE_ACCOUNTS=${unreadableAccounts}`);
console.log(`OPEN_LIVE_BROKER_POSITIONS=${openPositions}`);
console.log(`BROKER_EXPOSURE_CAPABLE_ORDERS=${activeOrders}`);
console.log(`BROKER_UNKNOWN_ORDERS=${unknownOrders}`);
console.log(`BROKER_PROVEN_SYNTHETIC_RECORDS=${syntheticOrders}`);
console.log(`ACTIVE_BROKER_CONDITIONAL_RULES=${activeConditionalRules}`);
console.log(`RULENIX_OWNED_POSITIONS=${exposureTotals.positions[ownership.rulenix]}`);
console.log(`RULENIX_OWNED_ACTIVE_ORDERS=${exposureTotals.orders[ownership.rulenix]}`);
console.log(`MANUAL_EXTERNAL_POSITIONS=${exposureTotals.positions[ownership.manual]}`);
console.log(`MANUAL_EXTERNAL_ACTIVE_ORDERS=${exposureTotals.orders[ownership.manual]}`);
console.log(`AMBIGUOUS_POSITIONS=${exposureTotals.positions[ownership.ambiguous]}`);
console.log(`AMBIGUOUS_ACTIVE_ORDERS=${exposureTotals.orders[ownership.ambiguous]}`);
console.log(`MANUAL_EXTERNAL_CONDITIONAL_RULES=${exposureTotals.conditionals[ownership.manual]}`);
console.log(`AMBIGUOUS_CONDITIONAL_RULES=${exposureTotals.conditionals[ownership.ambiguous]}`);
console.log(`OFFLINE_SAFE_USER_IDS=${offlineSafeUserIds.join(",")}`);
console.log(`DEPLOYMENT_GATE=${allowed ? "PASS" : "BLOCK"}`);
if (!allowed) process.exit(2);
