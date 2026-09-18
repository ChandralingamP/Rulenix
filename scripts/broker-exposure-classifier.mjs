const text = (item, ...names) => {
  for (const name of names) {
    if (item?.[name] !== undefined && item[name] !== null) return String(item[name]).trim();
  }
  return "";
};

const number = (item, ...names) => {
  const raw = text(item, ...names);
  if (raw === "") return undefined;
  const parsed = Number(raw);
  return Number.isFinite(parsed) ? parsed : undefined;
};

export const terminalStatuses = new Set([
  "complete", "completed", "filled", "cancelled", "canceled", "rejected", "expired",
]);

export function orderIdentity(item) {
  return {
    orderId: text(item, "orderid", "orderId"),
    exchangeOrderId: text(item, "exchangeorderid", "exchangeOrderId"),
    uniqueOrderId: text(item, "uniqueorderid", "uniqueOrderId"),
    symbol: text(item, "tradingsymbol", "tradingSymbol"),
    symbolToken: text(item, "symboltoken", "symbolToken"),
    exchange: text(item, "exchange").toUpperCase(),
    orderTag: text(item, "ordertag", "orderTag"),
    side: text(item, "transactiontype", "transactionType", "side").toUpperCase(),
    quantity: number(item, "quantity", "fillsize", "fillSize", "filledshares", "filledShares"),
  };
}

export const ownership = Object.freeze({
  rulenix: "RULENIX_OWNED",
  manual: "MANUAL_EXTERNAL",
  ambiguous: "AMBIGUOUS",
});

export function isRulenixOrderTag(value) {
  return /^RX[0-9A-F]{18}$/.test(String(value ?? "").trim().toUpperCase());
}

function knownOrderSets(knownOrders = []) {
  return {
    ids: new Set(knownOrders.map((item) => String(item.broker_order_id ?? "").trim()).filter(Boolean)),
    tags: new Set(knownOrders.map((item) => String(item.client_order_id ?? "").trim()).filter(Boolean)),
  };
}

export function classifyOrderOwnership(order, knownOrders = []) {
  const id = orderIdentity(order);
  const known = knownOrderSets(knownOrders);
  if ((id.orderId && known.ids.has(id.orderId)) || (id.orderTag && known.tags.has(id.orderTag))) {
    return { ownership: ownership.rulenix, evidence: "durable_local_order_identifier" };
  }
  if (isRulenixOrderTag(id.orderTag)) {
    return { ownership: ownership.ambiguous, evidence: "unmatched_rulenix_order_tag" };
  }
  const structurallyComplete = id.orderId !== "" && id.exchange !== ""
    && id.symbolToken !== "" && ["BUY", "SELL"].includes(id.side)
    && Number.isFinite(id.quantity) && id.quantity > 0;
  if (structurallyComplete) {
    return {
      ownership: ownership.manual,
      evidence: id.orderTag ? "complete_non_rulenix_broker_order_tag" : "complete_untagged_broker_order",
    };
  }
  return { ownership: ownership.ambiguous, evidence: "incomplete_unmatched_broker_order" };
}

function signedQuantity(item) {
  const id = orderIdentity(item);
  const fillQuantity = number(item, "fillsize", "fillSize", "filledshares", "filledShares", "quantity");
  if (!Number.isFinite(fillQuantity) || fillQuantity <= 0) return undefined;
  if (id.side === "BUY") return fillQuantity;
  if (id.side === "SELL") return -fillQuantity;
  return undefined;
}

export function classifyPositionOwnership(position, context = {}) {
  const id = orderIdentity(position);
  const net = number(position, "netqty", "netQty");
  if (!id.exchange || !id.symbolToken || !Number.isInteger(net) || net === 0) {
    return { ownership: ownership.ambiguous, evidence: "invalid_position_identity_or_quantity", details: { matchingFills: 0 } };
  }
  if ((context.openLocalPositions ?? []).some((local) =>
    String(local.exchange_segment ?? "").toUpperCase() === id.exchange
      && String(local.contract_token ?? "") === id.symbolToken)) {
    return { ownership: ownership.rulenix, evidence: "open_local_live_trade_contract", details: { matchingFills: 0 } };
  }
  const orderById = new Map((context.orders ?? []).map((order) => [orderIdentity(order).orderId, order]));
  const totals = { [ownership.rulenix]: 0, [ownership.manual]: 0, [ownership.ambiguous]: 0 };
  let matchingFills = 0;
  for (const fill of context.trades ?? []) {
    const fillId = orderIdentity(fill);
    if (fillId.exchange !== id.exchange || fillId.symbolToken !== id.symbolToken) continue;
    const signed = signedQuantity(fill);
    if (signed === undefined) {
      totals[ownership.ambiguous] += Math.sign(net);
      matchingFills += 1;
      continue;
    }
    const source = orderById.get(fillId.orderId) ?? fill;
    const attribution = classifyOrderOwnership(source, context.knownOrders);
    totals[attribution.ownership] += signed;
    matchingFills += 1;
  }
  if (matchingFills > 0 && totals[ownership.rulenix] === net
      && totals[ownership.manual] === 0 && totals[ownership.ambiguous] === 0) {
    return { ownership: ownership.rulenix, evidence: "net_position_matches_rulenix_fills", details: { matchingFills, ...totals } };
  }
  if (matchingFills > 0 && totals[ownership.manual] === net
      && totals[ownership.rulenix] === 0 && totals[ownership.ambiguous] === 0) {
    return { ownership: ownership.manual, evidence: "net_position_matches_manual_fills", details: { matchingFills, ...totals } };
  }
  return {
    ownership: ownership.ambiguous,
    evidence: "position_fill_ownership_not_exclusive",
    details: { matchingFills, ...totals },
  };
}

export function classifyConditionalOwnership(rule) {
  const id = orderIdentity(rule);
  const reference = text(rule, "id", "ruleid", "ruleId", "uniqueid", "uniqueId");
  const quantity = number(rule, "qty", "quantity");
  if (reference && id.exchange && id.symbolToken && Number.isFinite(quantity) && quantity > 0) {
    return { ownership: ownership.manual, evidence: "complete_external_conditional_rule" };
  }
  return { ownership: ownership.ambiguous, evidence: "incomplete_conditional_rule" };
}

function sameInstrument(left, right) {
  const a = orderIdentity(left);
  const b = orderIdentity(right);
  return (a.symbolToken !== "" && a.symbolToken === b.symbolToken)
    || (a.symbol !== "" && a.symbol === b.symbol);
}

function strictSyntheticShape(order) {
  const id = orderIdentity(order);
  return text(order, "status", "orderstatus", "orderStatus") === ""
    && id.orderId === ""
    && id.exchangeOrderId === ""
    && text(order, "parentorderid", "parentOrderId") === ""
    && id.uniqueOrderId.startsWith("SE-")
    && text(order, "ordertype", "orderType").toUpperCase() === "OCO_LIMIT"
    && text(order, "variety").toUpperCase() === "NORMAL"
    && text(order, "producttype", "productType").toUpperCase() === "INTRADAY"
    && /^N_Spark_(Android|IOS)_/.test(text(order, "strategycode", "strategyCode"))
    && (number(order, "quantity") ?? 0) > 0
    && number(order, "filledshares", "filledShares") === 0
    && number(order, "unfilledshares", "unfilledShares") === 0
    && number(order, "price") === 0
    && number(order, "triggerprice", "triggerPrice") === 0;
}

function hasExecutableSibling(order, orders) {
  const ownUniqueId = orderIdentity(order).uniqueOrderId;
  return orders.some((candidate) => {
    if (orderIdentity(candidate).uniqueOrderId === ownUniqueId || !sameInstrument(order, candidate)) return false;
    const status = text(candidate, "status", "orderstatus", "orderStatus").toLowerCase();
    if (terminalStatuses.has(status)) return false;
    const id = orderIdentity(candidate);
    return status !== ""
      || id.orderId !== ""
      || id.exchangeOrderId !== ""
      || (number(candidate, "unfilledshares", "unfilledShares") ?? 0) > 0
      || (number(candidate, "price") ?? 0) > 0
      || (number(candidate, "triggerprice", "triggerPrice") ?? 0) > 0;
  });
}

export function classifyBrokerOrder(order, context = {}) {
  const status = text(order, "status", "orderstatus", "orderStatus").toLowerCase();
  if (terminalStatuses.has(status)) return "terminal";
  if (status !== "") return "active";
  const identity = orderIdentity(order);
  const position = (context.positions ?? []).find((item) => sameInstrument(order, item));
  const positionNet = position && number(position, "netqty", "netQty");
  const detailsNotFound = context.detail?.httpStatus === 200
    && context.detail?.brokerStatus === false
    && context.detail?.errorCode === "AB1007";
  const noConditionalRule = context.conditionalReadSucceeded === true
    && !(context.conditionalRules ?? []).some((item) => sameInstrument(order, item));
  const noTradeForObject = !(context.trades ?? []).some((item) => {
    const tradeIdentity = orderIdentity(item);
    return (identity.orderId !== "" && tradeIdentity.orderId === identity.orderId)
      || (identity.uniqueOrderId !== "" && tradeIdentity.uniqueOrderId === identity.uniqueOrderId);
  });
  if (strictSyntheticShape(order) && positionNet === 0 && detailsNotFound && noConditionalRule
      && noTradeForObject && !hasExecutableSibling(order, context.orders ?? [])) return "synthetic";
  return "unknown";
}

export function conditionalRuleIsActive(rule) {
  const status = text(rule, "status", "ruleStatus", "rulestatus").toUpperCase();
  return !["CANCELLED", "CANCELED", "REJECTED", "EXPIRED", "COMPLETED", "COMPLETE"].includes(status);
}
