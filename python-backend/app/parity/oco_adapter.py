from __future__ import annotations

import re
from collections.abc import Mapping, Sequence
from datetime import datetime, timezone
from decimal import Decimal
from typing import Any

from ..reconciliation.domain import (
    BrokerFill,
    BrokerOrder,
    BrokerPosition,
    ConditionalRule,
    EvidenceStatus,
    ReadEvidence,
    is_synthetic_android_oco,
)


def _text(item: Mapping[str, Any], *names: str) -> str:
    for name in names:
        value = item.get(name)
        if value is not None:
            return str(value).strip()
    return ""


def _number(item: Mapping[str, Any], *names: str) -> float | None:
    raw = _text(item, *names)
    if not raw:
        return None
    try:
        return float(raw)
    except ValueError:
        return None


def _identity(item: Mapping[str, Any]) -> tuple[str, str, str]:
    return (
        _text(item, "orderid", "orderId"),
        _text(item, "uniqueorderid", "uniqueOrderId"),
        _text(item, "symboltoken", "symbolToken") or _text(item, "tradingsymbol", "tradingSymbol"),
    )


def _same_instrument(left: Mapping[str, Any], right: Mapping[str, Any]) -> bool:
    left_token = _text(left, "symboltoken", "symbolToken")
    right_token = _text(right, "symboltoken", "symbolToken")
    left_symbol = _text(left, "tradingsymbol", "tradingSymbol")
    right_symbol = _text(right, "tradingsymbol", "tradingSymbol")
    return bool((left_token and left_token == right_token) or (left_symbol and left_symbol == right_symbol))


def _strict_shape(order: Mapping[str, Any]) -> bool:
    return bool(
        not _text(order, "status", "orderstatus", "orderStatus")
        and not _text(order, "orderid", "orderId")
        and not _text(order, "exchangeorderid", "exchangeOrderId")
        and not _text(order, "parentorderid", "parentOrderId")
        and _text(order, "uniqueorderid", "uniqueOrderId").startswith("SE-")
        and _text(order, "ordertype", "orderType").upper() == "OCO_LIMIT"
        and _text(order, "variety").upper() == "NORMAL"
        and _text(order, "producttype", "productType").upper() == "INTRADAY"
        and re.match(r"^N_Spark_(Android|IOS)_", _text(order, "strategycode", "strategyCode")) is not None
        and (_number(order, "quantity") or 0) > 0
        and _number(order, "filledshares", "filledShares") == 0
        and _number(order, "unfilledshares", "unfilledShares") == 0
        and _number(order, "price") == 0
        and _number(order, "triggerprice", "triggerPrice") == 0
    )


def classify_oco_fixture(fixture: Mapping[str, Any]) -> dict[str, str]:
    order_raw = fixture["order"]
    context = fixture.get("context", {})
    order_id, unique_id, token = _identity(order_raw)
    canonical_order_id = order_id or unique_id
    symbol = _text(order_raw, "tradingsymbol", "tradingSymbol")
    order = BrokerOrder(
        order_id=canonical_order_id,
        token=token,
        symbol=symbol,
        android_synthetic=_strict_shape(order_raw),
        order_shape="android_synthetic_oco" if _strict_shape(order_raw) else "",
    )

    positions_status = EvidenceStatus.SUCCESS if context.get("positionsReadSucceeded") is True else EvidenceStatus.FAILED
    positions = [
        BrokerPosition(
            token=_text(item, "symboltoken", "symbolToken"),
            symbol=_text(item, "tradingsymbol", "tradingSymbol"),
            exchange="NFO",
            quantity=int(_number(item, "netqty", "netQty") or 0),
        )
        for item in context.get("positions", [])
        if _same_instrument(order_raw, item)
    ]
    if positions_status.successful and not positions:
        return {"classification": "unknown"}
    position_evidence: ReadEvidence[Sequence[BrokerPosition]] = ReadEvidence.success(positions) if positions_status.successful else ReadEvidence.failure(positions_status, "position read failed")

    detail = context.get("detail") or {}
    if detail.get("httpStatus") == 200 and detail.get("brokerStatus") is False and detail.get("errorCode") == "AB1007":
        individual: ReadEvidence[BrokerOrder | None] = ReadEvidence.failure(EvidenceStatus.FAILED, "AB1007 Order not found")
    elif detail.get("timedOut") is True:
        individual = ReadEvidence.failure(EvidenceStatus.TIMED_OUT, "timeout")
    else:
        individual = ReadEvidence.success(BrokerOrder(order_id="existing")) if detail.get("brokerStatus") is True else ReadEvidence.failure(EvidenceStatus.FAILED, str(detail.get("errorCode") or "individual read failed"))

    conditional_status = EvidenceStatus.SUCCESS if context.get("conditionalReadSucceeded") is True else EvidenceStatus.FAILED
    rules = [ConditionalRule(rule_id=str(index), order_id=canonical_order_id) for index, item in enumerate(context.get("conditionalRules", []), 1) if _same_instrument(order_raw, item)]
    conditional_evidence: ReadEvidence[Sequence[ConditionalRule]] = ReadEvidence.success(rules) if conditional_status.successful else ReadEvidence.failure(conditional_status, "conditional read failed")

    trade_status = EvidenceStatus.SUCCESS if context.get("tradeReadSucceeded") is True else EvidenceStatus.FAILED
    fills = [
        BrokerFill(
            str(index),
            _identity(item)[0] or _identity(item)[1],
            _identity(item)[2],
            _text(item, "tradingsymbol", "tradingSymbol"),
            "NFO",
            "SELL",
            1,
            Decimal(1),
            datetime.now(timezone.utc),
        )
        for index, item in enumerate(context.get("trades", []), 1)
    ]
    fill_evidence: ReadEvidence[Sequence[BrokerFill]] = ReadEvidence.success(fills) if trade_status.successful else ReadEvidence.failure(trade_status, "trade read failed")

    terminal = {"complete", "completed", "filled", "cancelled", "canceled", "rejected", "expired"}
    executable_sibling = any(
        _identity(item)[1] != _identity(order_raw)[1]
        and _same_instrument(order_raw, item)
        and _text(item, "status", "orderstatus", "orderStatus").lower() not in terminal
        and bool(
            _text(item, "status", "orderstatus", "orderStatus")
            or _identity(item)[0]
            or _text(item, "exchangeorderid", "exchangeOrderId")
            or (_number(item, "unfilledshares", "unfilledShares") or 0) > 0
            or (_number(item, "price") or 0) > 0
            or (_number(item, "triggerprice", "triggerPrice") or 0) > 0
        )
        for item in context.get("orders", [])
    )
    synthetic = is_synthetic_android_oco(
        order=order,
        position=position_evidence,
        individual=individual,
        conditional_rules=conditional_evidence,
        fills=fill_evidence,
        executable_sibling=executable_sibling,
    )
    return {"classification": "synthetic" if synthetic else "unknown"}
