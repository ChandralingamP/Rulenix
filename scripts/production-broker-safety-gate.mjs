import { createDecipheriv } from "node:crypto";
import http from "node:http";
import https from "node:https";
import {
  classifyBrokerOrder,
  conditionalRuleIsActive,
  orderIdentity,
} from "./broker-exposure-classifier.mjs";
import {
  deploymentAccountDecision,
  platformDeploymentAllowed,
} from "./production-broker-safety-gate-lib.mjs";
import {
  brokerAccountRef,
  credentialFingerprint,
  readDiagnostic,
  strictAuthoritativeGate,
} from "./production-broker-read-diagnostics.mjs";

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
        resolve({
          httpStatus: response.statusCode ?? 0,
          payload: parsed,
          localAddress: response.socket?.localAddress,
        });
      });
    });
    request.on("timeout", () => request.destroy(new Error("broker read timeout")));
    request.on("error", reject);
    if (payload !== undefined) request.write(payload);
    request.end();
  });
}

async function brokerRequest(
  operation,
  path,
  apiKey,
  jwtToken,
  localAddress,
  diagnostics,
  method = "GET",
  body,
) {
  const response = await rawRequest(path, apiKey, jwtToken, localAddress, method, body);
  diagnostics.push(readDiagnostic({
    operation,
    path,
    method,
    expectedLocalAddress: localAddress,
    response,
    secrets: [apiKey, jwtToken],
  }));
  if (response.httpStatus < 200 || response.httpStatus >= 300 || response.payload?.status !== true) {
    const code = String(response.payload?.errorcode ?? `http_${response.httpStatus}`).replace(/[^a-zA-Z0-9_.-]/g, "_");
    throw new Error(`broker read failed (${code})`);
  }
  return Array.isArray(response.payload.data) ? response.payload.data : [];
}

async function brokerDetail(uniqueOrderId, apiKey, jwtToken, localAddress, diagnostics) {
  const path = `/rest/secure/angelbroking/order/v1/details/${encodeURIComponent(uniqueOrderId)}`;
  const response = await rawRequest(
    path,
    apiKey,
    jwtToken,
    localAddress,
  );
  diagnostics.push(readDiagnostic({
    operation: "individual_order",
    path,
    method: "GET",
    expectedLocalAddress: localAddress,
    response,
    secrets: [apiKey, jwtToken],
  }));
  return {
    httpStatus: response.httpStatus,
    brokerStatus: response.payload?.status,
    errorCode: String(response.payload?.errorcode ?? ""),
  };
}

async function allConditionalRules(apiKey, jwtToken, localAddress, diagnostics) {
  const rules = [];
  for (let page = 1; page <= 100; page += 1) {
    const batch = await brokerRequest(
      "conditional_gtt_inventory",
      "/rest/secure/angelbroking/gtt/v1/ruleList",
      apiKey,
      jwtToken,
      localAddress,
      diagnostics,
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

for (let index = 0; index < accounts.length; index += 1) {
  const account = accounts[index];
  let apiKey = "";
  let jwtToken = "";
  let brokerReadable = false;
  let brokerSafe = false;
  let brokerExposureObserved = false;
  let brokerCounts = null;
  let diagnostic = "";
  const requestDiagnostics = [];
  let apiKeyFingerprint = "missing";
  let jwtTokenFingerprint = "missing";
  try {
    if (account.egress_ip && (account.egress_configuration_status !== "CONFIGURED"
      || account.egress_verification_status !== "VERIFIED")) {
      throw new Error("configured egress assignment is unavailable");
    }
    const localAddress = bindingAddress(account.egress_ip);
    apiKey = decrypt(account.user_id, "api_key", account.secrets?.api_key);
    jwtToken = decrypt(account.user_id, "jwt_token", account.secrets?.jwt_token);
    apiKeyFingerprint = credentialFingerprint(apiKey);
    jwtTokenFingerprint = credentialFingerprint(jwtToken);
    const reads = await Promise.allSettled([
      brokerRequest("order_book", "/rest/secure/angelbroking/order/v1/getOrderBook", apiKey, jwtToken, localAddress, requestDiagnostics),
      brokerRequest("positions", "/rest/secure/angelbroking/order/v1/getPosition", apiKey, jwtToken, localAddress, requestDiagnostics),
      brokerRequest("trade_book", "/rest/secure/angelbroking/order/v1/getTradeBook", apiKey, jwtToken, localAddress, requestDiagnostics),
      allConditionalRules(apiKey, jwtToken, localAddress, requestDiagnostics),
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
          detail = await brokerDetail(
            identity.uniqueOrderId,
            apiKey,
            jwtToken,
            localAddress,
            requestDiagnostics,
          );
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
      if (classification === "active") accountActiveOrders += 1;
      if (classification === "unknown") accountUnknownOrders += 1;
      if (classification === "synthetic") accountSyntheticOrders += 1;
    }
    const accountActiveConditionalRules = conditionalRules.filter(conditionalRuleIsActive).length;
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
    brokerSafe = brokerReadable && noObservedExposure;
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
    local: account.local,
  });
  decisions.push(decision);
  if (!brokerReadable && decision.allow) offlineSafeUserIds.push(account.user_id);
  console.log(`ACCOUNT_${index + 1}=${JSON.stringify({
    user_id: account.user_id,
    username: account.username,
    broker_readable: brokerReadable,
    broker_safe: brokerSafe,
    broker_account_ref: brokerAccountRef(account.broker_account_id),
    broker_credential_revision: account.broker_credential_revision,
    token_state: account.token_state,
    last_token_status: account.last_token_status,
    last_token_check_at: account.last_token_check_at,
    api_key_fingerprint: apiKeyFingerprint,
    api_key_storage_version: account.secrets?.api_key?.version ?? null,
    api_key_updated_at: account.secrets?.api_key?.updated_at ?? null,
    jwt_token_fingerprint: jwtTokenFingerprint,
    jwt_token_storage_version: account.secrets?.jwt_token?.version ?? null,
    jwt_token_updated_at: account.secrets?.jwt_token?.updated_at ?? null,
    assigned_egress_ip: account.egress_ip ?? "default",
    egress_configuration_status: account.egress_configuration_status ?? "default",
    egress_verification_status: account.egress_verification_status ?? "default",
    egress_last_verified_at: account.egress_last_verified_at ?? null,
    expected_local_address: bindingAddress(account.egress_ip) ?? "default",
    request_diagnostics: requestDiagnostics,
    local_unresolved: decision.localUnresolved,
    deployment_allowed: decision.allow,
    live_ready: decision.liveReady,
    classification: decision.classification,
    broker: brokerCounts,
    read_diagnostic: diagnostic,
  })}`);
}

for (const key of keys.values()) key.fill(0);
const authoritativeReadsRequired = process.env.PHASE11_REQUIRE_AUTHORITATIVE_BROKER_READS === "true";
const allowed = accounts.length > 0 && strictAuthoritativeGate({
  platformAllowed: platformDeploymentAllowed(decisions),
  unreadableAccounts,
  required: authoritativeReadsRequired,
});
console.log(`AUTHORITATIVE_BROKER_READS_REQUIRED=${authoritativeReadsRequired}`);
console.log(`BROKER_ACCOUNTS_CHECKED=${accounts.length}`);
console.log(`BROKER_READABLE_ACCOUNTS=${readableAccounts}`);
console.log(`BROKER_UNREADABLE_ACCOUNTS=${unreadableAccounts}`);
console.log(`OPEN_LIVE_BROKER_POSITIONS=${openPositions}`);
console.log(`BROKER_EXPOSURE_CAPABLE_ORDERS=${activeOrders}`);
console.log(`BROKER_UNKNOWN_ORDERS=${unknownOrders}`);
console.log(`BROKER_PROVEN_SYNTHETIC_RECORDS=${syntheticOrders}`);
console.log(`ACTIVE_BROKER_CONDITIONAL_RULES=${activeConditionalRules}`);
console.log(`OFFLINE_SAFE_USER_IDS=${offlineSafeUserIds.join(",")}`);
console.log(`DEPLOYMENT_GATE=${allowed ? "PASS" : "BLOCK"}`);
if (!allowed) process.exit(2);
