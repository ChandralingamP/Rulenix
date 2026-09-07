from decimal import Decimal
from uuid import UUID

from sqlalchemy.ext.asyncio import AsyncSession

from .domain import FillApplication, IntentStatus, OrderStatus, ProtectionStatus, Side
from .repository import TradingRepository


class TradingDomainService:
    """Durable trading state operations only; no broker client is accepted here."""

    def __init__(self, session: AsyncSession):
        self.repository = TradingRepository(session)

    async def transition_order(self, order_id: UUID, user_id: UUID, expected: OrderStatus, target: OrderStatus) -> int:
        return await self.repository.transition_order(order_id, user_id, expected, target)

    async def transition_safety(self, trade_id: UUID, user_id: UUID, expected: ProtectionStatus, target: ProtectionStatus) -> int:
        return await self.repository.transition_safety(trade_id, user_id, expected, target)

    async def claim_entry_work(self, limit: int = 100):
        return await self.repository.claim_execution_intents(limit=limit)

    async def complete_entry_work(self, intent_id: UUID, user_id: UUID, status: IntentStatus, *, error: str = "", strategy_order_id: UUID | None = None) -> bool:
        return await self.repository.complete_intent(intent_id, user_id, status, error=error, strategy_order_id=strategy_order_id)

    async def ingest_fill(self, order_id: UUID, user_id: UUID, observed_cumulative: int, fill_price: Decimal | str) -> FillApplication | None:
        return await self.repository.apply_order_fill(order_id=order_id, user_id=user_id, observed_cumulative=observed_cumulative, fill_price=fill_price)

    async def request_manual_close(self, trade_id: UUID, user_id: UUID, quantity: int) -> bool:
        return await self.repository.request_manual_close(trade_id=trade_id, user_id=user_id, requested_quantity=quantity)

    async def queue_sl2_reversal(self, trade_id: UUID, user_id: UUID, snapshot_id: UUID, instrument: str, source_direction: Side, lots: int, fill_price: Decimal | str) -> bool:
        return await self.repository.create_sl2_reversal_intent(source_trade_id=trade_id, user_id=user_id, snapshot_id=snapshot_id, instrument=instrument, source_direction=source_direction, lots=lots, entry_price=fill_price)
