"""Intent orchestration and typed broker-action construction.

LIVE actions stop at the Phase 3 mutation guard.  No method in this module owns
an Angel transport or fabricates a broker order identifier.
"""

from dataclasses import dataclass
from decimal import Decimal
from enum import StrEnum
from typing import Any
from uuid import UUID

from sqlalchemy import text
from sqlalchemy.ext.asyncio import AsyncSession

from app.broker.mutations import LiveMutationCoordinator, MutationState
from app.risk import ActionKind, RiskSafetyService, SafetyDecision, SafetyRequest
from app.trading.domain import IntentStatus
from app.trading.repository import ClaimedIntent, TradingRepository


class ExecutionOutcome(StrEnum):
    DEMO_SIMULATED = "DEMO_SIMULATED"
    LIVE_MUTATION_DISABLED_DURING_MIGRATION = "LIVE_MUTATION_DISABLED_DURING_MIGRATION"
    BLOCKED = "BLOCKED"
    LIVE_SUBMITTED = "LIVE_SUBMITTED"
    LIVE_AMBIGUOUS = "LIVE_AMBIGUOUS"
    LIVE_FAILED = "LIVE_FAILED"


@dataclass(frozen=True)
class BrokerActionRequest:
    action: str
    exchange: str
    symbol: str
    token: str
    side: str
    order_type: str
    product_type: str
    variety: str
    quantity: int
    price: Decimal
    trigger_price: Decimal | None
    duration: str = "DAY"
    client_reference: str = ""


@dataclass(frozen=True)
class ExecutionResult:
    outcome: ExecutionOutcome
    decision: SafetyDecision
    request: BrokerActionRequest | None
    intent_id: UUID | None
    broker_order_id: str | None = None


def _action_kind(action: str, role: str) -> ActionKind:
    if action == "SL2_REVERSAL":
        return ActionKind.SL2_REVERSAL
    if action == "SQUARE_OFF":
        return ActionKind.EOD_SQUARE_OFF
    if role == "SL1":
        return ActionKind.STOP_LOSS
    if role == "SL2":
        return ActionKind.STOP_LOSS
    if role == "TARGET":
        return ActionKind.TARGET
    if role == "EMERGENCY_CLOSE":
        return ActionKind.EMERGENCY_CLOSE
    if action == "MANUAL_CLOSE":
        return ActionKind.MANUAL_CLOSE
    return ActionKind.ENTRY


class ExecutionOrchestrator:
    def __init__(self, session: AsyncSession, live: LiveMutationCoordinator | None = None):
        self.session = session
        self.trading = TradingRepository(session)
        self.safety = RiskSafetyService(session)
        self.live = live

    @staticmethod
    def build_request(
        intent: ClaimedIntent | Any,
        *,
        exchange: str = "",
        symbol: str = "",
        token: str = "",
        product_type: str = "INTRADAY",
        price: Decimal | str | None = None,
        trigger_price: Decimal | str | None = None,
    ) -> BrokerActionRequest:
        role, side = (
            str(intent.role),
            "BUY" if str(intent.role) in {"BUY_ENTRY", "TARGET", "SL1", "SL2"} else "SELL",
        )
        if hasattr(intent, "side"):
            side = str(intent.side)
        value = Decimal(str(price if price is not None else getattr(intent, "price", "0")))
        trigger = (
            trigger_price if trigger_price is not None else getattr(intent, "trigger_price", None)
        )
        return BrokerActionRequest(
            str(getattr(intent, "action", "ENTRY")),
            exchange,
            symbol,
            token,
            side,
            "MARKET" if role in {"TARGET", "SL1", "SL2", "EMERGENCY_CLOSE"} else "STOPLOSS",
            product_type,
            "NORMAL",
            int(getattr(intent, "quantity", 0)),
            value,
            Decimal(str(trigger)) if trigger is not None else None,
            client_reference=f"rulenix-{getattr(intent, 'id', '')}",
        )

    async def process_claimed(
        self,
        intent: Any,
        *,
        exchange: str = "",
        symbol: str = "",
        token: str = "",
        product_type: str = "INTRADAY",
    ) -> ExecutionResult:
        kind = _action_kind(str(intent.action), str(intent.role))
        mode = str(getattr(intent, "execution_mode", "demo"))
        request = SafetyRequest(
            user_id=UUID(str(intent.user_id)),
            action=kind,
            execution_mode=mode,
            intent_id=UUID(str(intent.id)),
            trade_id=UUID(str(intent.trade_id)) if getattr(intent, "trade_id", None) else None,
            quantity=int(getattr(intent, "quantity", 0) or 0),
            attributable_quantity=int(getattr(intent, "quantity", 0) or 0),
            lots=int(getattr(intent, "lots", 0) or 0),
            strategy_key=getattr(intent, "strategy_key", None),
            instrument=getattr(intent, "instrument", None),
            role=str(getattr(intent, "role", "") or ""),
            session_key=str(getattr(intent, "session_key", "") or ""),
        )
        decision = await self.safety.final_pre_mutation_check(request)
        if not decision.allowed:
            await self.trading.complete_intent(
                UUID(str(intent.id)),
                UUID(str(intent.user_id)),
                IntentStatus.RETRY_WAIT,
                error=decision.reason_code.value,
            )
            await self.session.commit()
            return ExecutionResult(ExecutionOutcome.BLOCKED, decision, None, UUID(str(intent.id)))
        action_request = self.build_request(
            intent, exchange=exchange, symbol=symbol, token=token, product_type=product_type
        )
        if mode == "demo":
            order_id = await self.trading.reserve_order_for_intent(
                intent_id=UUID(str(intent.id)), user_id=UUID(str(intent.user_id))
            )
            order = (
                (
                    await self.session.execute(
                        text("""
                        SELECT order_type,quantity,price FROM strategy_orders
                         WHERE id=:order AND user_id=:user FOR UPDATE
                        """),
                        {"order": order_id, "user": intent.user_id},
                    )
                )
                .mappings()
                .one()
            )
            await self.session.execute(
                text("""
                UPDATE strategy_orders SET status='submitted',broker_order_id=:broker,
                  broker_status='Demo order accepted locally',updated_at=NOW()
                 WHERE id=:order AND status='pending'
                """),
                {"order": order_id, "broker": f"DEMO-{order_id}"},
            )
            if str(order["order_type"]).upper() == "MARKET":
                await self.session.execute(
                    text("""
                    UPDATE strategy_orders SET status='filled',filled_quantity=quantity,
                      average_fill_price=price,filled_price=price,filled_at=NOW(),
                      broker_status='Demo market order filled locally',updated_at=NOW()
                     WHERE id=:order AND status='submitted'
                    """),
                    {"order": order_id},
                )
            await self.trading.complete_intent(
                UUID(str(intent.id)),
                UUID(str(intent.user_id)),
                IntentStatus.SUBMITTED,
                strategy_order_id=order_id,
            )
            await self.session.commit()
            return ExecutionResult(
                ExecutionOutcome.DEMO_SIMULATED,
                decision,
                action_request,
                UUID(str(intent.id)),
                f"DEMO-{order_id}",
            )
        if self.live is not None:
            order_id = await self.trading.reserve_order_for_intent(
                intent_id=UUID(str(intent.id)), user_id=UUID(str(intent.user_id))
            )
            await self.session.commit()
            outcome = await self.live.place_order(order_id)
            if outcome.state is MutationState.ACKNOWLEDGED:
                intent_status = IntentStatus.SUBMITTED
                execution_outcome = ExecutionOutcome.LIVE_SUBMITTED
            elif outcome.state is MutationState.AMBIGUOUS:
                intent_status = IntentStatus.SUBMITTED
                execution_outcome = ExecutionOutcome.LIVE_AMBIGUOUS
            else:
                intent_status = IntentStatus.FAILED
                execution_outcome = ExecutionOutcome.LIVE_FAILED
            await self.trading.complete_intent(
                UUID(str(intent.id)),
                UUID(str(intent.user_id)),
                intent_status,
                error="" if outcome.error_category is None else outcome.error_category.value,
                strategy_order_id=order_id,
            )
            await self.session.commit()
            return ExecutionResult(
                execution_outcome,
                decision,
                action_request,
                UUID(str(intent.id)),
                outcome.broker_order_id or None,
            )
        await self.trading.complete_intent(
            UUID(str(intent.id)),
            UUID(str(intent.user_id)),
            IntentStatus.RETRY_WAIT,
            error=ExecutionOutcome.LIVE_MUTATION_DISABLED_DURING_MIGRATION.value,
        )
        await self.session.commit()
        return ExecutionResult(
            ExecutionOutcome.LIVE_MUTATION_DISABLED_DURING_MIGRATION,
            decision,
            action_request,
            UUID(str(intent.id)),
        )

    async def process_due(self, *, limit: int = 100, signal_id: UUID | None = None) -> list[ExecutionResult]:
        """Claim durable ENTRY work and process each claim exactly once."""
        claimed = await self.trading.claim_execution_intents(limit=limit, signal_id=signal_id)
        results: list[ExecutionResult] = []
        for item in claimed:
            row = (
                (
                    await self.session.execute(
                        text("""
                        SELECT i.id,i.user_id,i.trade_id,i.action,i.role,i.side,i.lots,i.quantity,i.price,i.trigger_price,
                               i.strategy_key,i.instrument,i.session_key,COALESCE(p.trading_mode,'demo') AS execution_mode
                          FROM strategy_execution_intents i
                          LEFT JOIN user_profiles p ON p.user_id=i.user_id
                         WHERE id=:id AND status='claimed'
                    """),
                        {"id": item.id},
                    )
                )
                .mappings()
                .first()
            )
            if row is not None:
                results.append(await self.process_claimed(row))
        return results


__all__ = ["BrokerActionRequest", "ExecutionOrchestrator", "ExecutionOutcome", "ExecutionResult"]
