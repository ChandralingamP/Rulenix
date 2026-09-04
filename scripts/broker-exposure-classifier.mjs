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
  };
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
