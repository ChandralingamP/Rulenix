from dataclasses import dataclass
from datetime import datetime
from decimal import Decimal
from typing import Any
from uuid import NAMESPACE_URL, UUID, uuid5

from sqlalchemy import text
from sqlalchemy.ext.asyncio import AsyncSession

from .domain import (
    FillApplication,
    IntentStatus,
    OrderStatus,
    ProtectionStatus,
    Side,
    TradeStatus,
    apply_fill,
    sl2_reversal,
    validate_intent_transition,
    validate_order_transition,
    validate_safety_transition,
)


class TradingRepositoryError(RuntimeError):
    pass


class OwnershipError(TradingRepositoryError):
    pass


class ConcurrentStateError(TradingRepositoryError):
    pass


@dataclass(frozen=True)
class ClaimedIntent:
    id: UUID
    user_id: UUID
    signal_id: UUID
    action: str
    role: str
    status: IntentStatus
    attempts: int
    claimed_at: datetime


class TradingRepository:
    """Explicit repository for Rust-owned tables; no method transmits to Angel."""

    def __init__(self, session: AsyncSession):
        self.session = session

    async def transition_order(self, order_id: UUID, user_id: UUID, expected: str | OrderStatus, target: str | OrderStatus, *, broker_status: str | None = None) -> int:
        validate_order_transition(expected, target)
        expected_value = expected.value if isinstance(expected, OrderStatus) else expected
        target_value = target.value if isinstance(target, OrderStatus) else target
        values: dict[str, Any] = {"id": order_id, "user": user_id, "expected": expected_value, "target": target_value, "broker_status": broker_status or ""}
        result = await self.session.execute(text("""
            UPDATE strategy_orders
               SET status=:target, broker_status=CASE WHEN :broker_status='' THEN broker_status ELSE :broker_status END,
                   state_version=state_version+1, updated_at=NOW()
             WHERE id=:id AND user_id=:user AND status=:expected
             RETURNING state_version
        """), values)
        row = result.mappings().first()
        if row is not None:
            return int(row["state_version"])
        exists = (await self.session.execute(text("SELECT user_id,status FROM strategy_orders WHERE id=:id"), {"id": order_id})).mappings().first()
        if exists is None or exists["user_id"] != user_id:
            raise OwnershipError("Order is not owned by this user.")
        raise ConcurrentStateError(f"Order state changed before {expected} -> {target} could be committed.")

    async def transition_trade(self, trade_id: UUID, user_id: UUID, expected: str | TradeStatus, target: str | TradeStatus) -> None:
        old, new = TradeStatus(expected), TradeStatus(target)
        if old is TradeStatus.CLOSED and new is not old:
            raise ValueError("Closed trade cannot regress.")
        result = await self.session.execute(text("UPDATE trades SET status=:target,updated_at=NOW() WHERE id=:id AND user_id=:user AND status=:expected"), {"id": trade_id, "user": user_id, "expected": old.value, "target": new.value})
        if getattr(result, "rowcount", 0) != 1:
            exists = (await self.session.execute(text("SELECT user_id,status FROM trades WHERE id=:id"), {"id": trade_id})).mappings().first()
            if exists is None or exists["user_id"] != user_id:
                raise OwnershipError("Trade is not owned by this user.")
            raise ConcurrentStateError("Trade state changed concurrently.")

    async def transition_safety(self, trade_id: UUID, user_id: UUID, expected: str | ProtectionStatus, target: str | ProtectionStatus) -> int:
        validate_safety_transition(expected, target)
        expected_value = expected.value if isinstance(expected, ProtectionStatus) else expected
        target_value = target.value if isinstance(target, ProtectionStatus) else target
        result = await self.session.execute(text("UPDATE trades SET safety_status=:target,updated_at=NOW() WHERE id=:id AND user_id=:user AND safety_status=:expected RETURNING id"), {"id": trade_id, "user": user_id, "expected": expected_value, "target": target_value})
        row = result.mappings().first()
        if row is None:
            exists = (await self.session.execute(text("SELECT user_id,safety_status FROM trades WHERE id=:id"), {"id": trade_id})).mappings().first()
            if exists is None or exists["user_id"] != user_id:
                raise OwnershipError("Trade is not owned by this user.")
            raise ConcurrentStateError("Trade safety state changed concurrently.")
        return 1

    async def claim_execution_intents(self, *, limit: int = 100, signal_id: UUID | None = None) -> list[ClaimedIntent]:
        if not 1 <= limit <= 100:
            raise ValueError("Claim limit must be between 1 and 100.")
        rows = (await self.session.execute(text("""
            UPDATE strategy_execution_intents
               SET status='claimed', attempts=attempts+1, claimed_at=NOW(), updated_at=NOW()
             WHERE id IN (
               SELECT id FROM strategy_execution_intents
                WHERE action='ENTRY' AND status IN ('pending','retry_wait') AND next_attempt_at<=NOW()
                  AND (CAST(:signal_id AS uuid) IS NULL OR signal_id=CAST(:signal_id AS uuid))
                ORDER BY created_at FOR UPDATE SKIP LOCKED LIMIT :limit
             )
             RETURNING id,user_id,signal_id,action,role,status,attempts,claimed_at
        """), {"limit": limit, "signal_id": signal_id})).mappings().all()
        return [ClaimedIntent(UUID(str(row["id"])), UUID(str(row["user_id"])), UUID(str(row["signal_id"])), str(row["action"]), str(row["role"]), IntentStatus(str(row["status"])), int(row["attempts"]), row["claimed_at"]) for row in rows]

    async def recover_stale_claims(self, *, age_seconds: int = 120) -> int:
        if age_seconds < 1:
            raise ValueError("Claim age must be positive.")
        result = await self.session.execute(text("UPDATE strategy_execution_intents SET status='retry_wait',next_attempt_at=NOW(),last_error='Execution claim heartbeat expired; safe retry queued.',updated_at=NOW() WHERE action='ENTRY' AND status='claimed' AND claimed_at<NOW()-(:age * INTERVAL '1 second')"), {"age": age_seconds})
        return int(getattr(result, "rowcount", 0))

    async def complete_intent(self, intent_id: UUID, user_id: UUID, status: str | IntentStatus, *, error: str = "", strategy_order_id: UUID | None = None) -> bool:
        final = IntentStatus(status)
        validate_intent_transition(IntentStatus.CLAIMED, final)
        result = await self.session.execute(text("""
            UPDATE strategy_execution_intents
               SET status=CAST(:status AS varchar(16)),strategy_order_id=COALESCE(:order_id,strategy_order_id),last_error=:error,
                   completed_at=CASE WHEN CAST(:status AS varchar(16)) IN ('completed','skipped','failed','expired') THEN NOW() ELSE completed_at END,
                   updated_at=NOW()
             WHERE id=:id AND user_id=:user AND status='claimed'
        """), {"id": intent_id, "user": user_id, "status": final.value, "error": error[:2000], "order_id": strategy_order_id})
        return bool(getattr(result, "rowcount", 0))

    async def reserve_order_for_intent(self, *, intent_id: UUID, user_id: UUID) -> UUID:
        row = (await self.session.execute(text("""
            SELECT i.id,i.user_id,i.snapshot_id,i.trade_id,i.session_key,i.role,i.side,i.status,
                   i.order_type,i.lots,i.quantity,i.price,i.trigger_price,i.strategy_order_id,
                   COALESCE(p.trading_mode,'demo') AS execution_mode,
                   s.exchange_segment,s.product_type
              FROM strategy_execution_intents i
              JOIN strategy_market_snapshots s ON s.id=i.snapshot_id
              LEFT JOIN user_profiles p ON p.user_id=i.user_id
             WHERE i.id=:intent FOR UPDATE OF i
        """), {"intent": intent_id})).mappings().first()
        if row is None or row["user_id"] != user_id:
            raise OwnershipError("Execution intent is not owned by this user.")
        if row["strategy_order_id"]:
            return UUID(str(row["strategy_order_id"]))
        if row["status"] != "claimed":
            raise ConcurrentStateError("Execution intent is not claimed.")
        quantity = int(row["quantity"] or 0)
        if quantity <= 0:
            raise ValueError("Execution intent quantity must be positive before order reservation.")
        order_id = uuid5(NAMESPACE_URL, f"rulenix:execution-intent:{intent_id}")
        client_order_id = f"RX{order_id.hex[:18].upper()}"
        await self.session.execute(text("""
            INSERT INTO strategy_orders(
                id,user_id,snapshot_id,trade_id,session_key,role,side,execution_mode,
                lots,quantity,price,trigger_price,status,idempotency_key,client_order_id,
                order_type,exchange_segment,product_type)
            VALUES(:id,:user,:snapshot,:trade,:session,:role,:side,:mode,:lots,:quantity,
                   :price,:trigger,'pending',:key,:client,:order_type,:exchange,:product)
            ON CONFLICT(idempotency_key) DO NOTHING
        """), {
            "id": order_id, "user": user_id, "snapshot": row["snapshot_id"],
            "trade": row["trade_id"], "session": str(row["session_key"])[:32],
            "role": row["role"], "side": row["side"], "mode": row["execution_mode"],
            "lots": row["lots"], "quantity": quantity, "price": row["price"],
            "trigger": row["trigger_price"], "key": f"python:intent:{intent_id}",
            "client": client_order_id, "order_type": row["order_type"],
            "exchange": row["exchange_segment"], "product": row["product_type"],
        })
        result = await self.session.execute(text("""
            UPDATE strategy_execution_intents SET strategy_order_id=:order,updated_at=NOW()
             WHERE id=:intent AND user_id=:user AND status='claimed'
               AND strategy_order_id IS NULL
        """), {"order": order_id, "intent": intent_id, "user": user_id})
        if getattr(result, "rowcount", 0) != 1:
            raise ConcurrentStateError("Execution intent order was reserved concurrently.")
        return order_id

    async def create_sl2_reversal_intent(self, *, source_trade_id: UUID, user_id: UUID, snapshot_id: UUID, instrument: str, source_direction: str | Side, lots: int, entry_price: Decimal | str) -> bool:
        source = (await self.session.execute(text("SELECT user_id,status,direction,exit_reason FROM trades WHERE id=:trade FOR UPDATE"), {"trade": source_trade_id})).mappings().first()
        if source is None or source["user_id"] != user_id:
            raise OwnershipError("The SL2 source trade is not owned by this user.")
        if source["status"] != "closed" or source["exit_reason"] != "SL2":
            raise ValueError("SL2 reversal requires a confirmed terminal SL2 source trade.")
        if source["direction"] != str(Side(source_direction)):
            raise ValueError("SL2 reversal direction does not match the source trade.")
        plan = sl2_reversal(source_direction, lots)
        if plan is None:
            raise ValueError("SL2 reversal requires a positive original lot count.")
        direction, _, planned_lots = plan
        result = await self.session.execute(text("""
            INSERT INTO strategy_reversal_intents(source_trade_id,user_id,snapshot_id,instrument,source_direction,reversal_direction,lots,entry_price,order_session_key)
            VALUES(:trade,:user,:snapshot,:instrument,:source,:reversal,:lots,:price,:session)
            ON CONFLICT (source_trade_id) DO NOTHING
        """), {"trade": source_trade_id, "user": user_id, "snapshot": snapshot_id, "instrument": instrument, "source": str(Side(source_direction)), "reversal": direction.value, "lots": planned_lots, "price": Decimal(str(entry_price)), "session": f"r-{source_trade_id.hex[:30]}"})
        return bool(getattr(result, "rowcount", 0))

    async def request_manual_close(self, *, trade_id: UUID, user_id: UUID, requested_quantity: int) -> bool:
        if requested_quantity <= 0:
            raise ValueError("Manual close quantity must be positive.")
        trade = (await self.session.execute(text("SELECT user_id,status,quantity,direction FROM trades WHERE id=:trade FOR UPDATE"), {"trade": trade_id})).mappings().first()
        if trade is None or trade["user_id"] != user_id:
            raise OwnershipError("Trade is not owned by this user.")
        if trade["status"] != "open" or requested_quantity > int(trade["quantity"]):
            raise ValueError("Manual close must target an open owned quantity.")
        close_side = Side(str(trade["direction"])).opposite.value
        result = await self.session.execute(text("""
            INSERT INTO manual_trade_close_intents(trade_id,user_id,status,requested_quantity,close_side)
            VALUES(:trade,:user,'requested',:quantity,:side)
            ON CONFLICT (trade_id) DO NOTHING
        """), {"trade": trade_id, "user": user_id, "quantity": requested_quantity, "side": close_side})
        return bool(getattr(result, "rowcount", 0))

    async def apply_order_fill(self, *, order_id: UUID, user_id: UUID, observed_cumulative: int, fill_price: Decimal | str) -> FillApplication | None:
        row = (await self.session.execute(text("SELECT user_id,quantity,filled_quantity,processed_quantity,average_fill_price,status FROM strategy_orders WHERE id=:id FOR UPDATE"), {"id": order_id})).mappings().first()
        if row is None or row["user_id"] != user_id:
            raise OwnershipError("Order is not owned by this user.")
        if row["status"] not in {"submitted", "partially_filled"}:
            return None
        application = apply_fill(order_quantity=int(row["quantity"]), processed_quantity=int(row["processed_quantity"]), filled_quantity=int(row["filled_quantity"]), average_price=row["average_fill_price"], observed_cumulative=observed_cumulative, fill_price=fill_price)
        if application.delta_quantity == 0:
            return application
        await self.session.execute(text("UPDATE strategy_orders SET status=:status,filled_quantity=:filled,processed_quantity=:processed,average_fill_price=:average,state_version=state_version+1,updated_at=NOW() WHERE id=:id"), {"status": application.status.value, "filled": application.cumulative_quantity, "processed": application.cumulative_quantity, "average": application.average_price, "id": order_id})
        return application
