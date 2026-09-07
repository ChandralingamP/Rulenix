"""Application-facing risk service.

The service exposes initial and final checks as separate calls so callers cannot
mistake an earlier approval for permission to mutate a broker account.
"""

from uuid import UUID

from sqlalchemy.ext.asyncio import AsyncSession

from .domain import SafetyDecision, SafetyRequest
from .repository import SafetyRepository


class RiskSafetyService:
    def __init__(self, session: AsyncSession):
        self.repository = SafetyRepository(session)

    async def evaluate(self, request: SafetyRequest) -> SafetyDecision:
        return await self.repository.final_pre_mutation_check(request)

    async def final_pre_mutation_check(self, request: SafetyRequest) -> SafetyDecision:
        """Always reload current state; there is intentionally no approval token."""
        return await self.repository.final_pre_mutation_check(request)

    async def final_pre_mutation_check_for(
        self,
        *,
        user_id: UUID,
        action: str,
        execution_mode: str = "live",
        trade_id: UUID | None = None,
        intent_id: UUID | None = None,
        quantity: int = 0,
        attributable_quantity: int = 0,
        lots: int = 0,
        strategy_key: str | None = None,
        instrument: str | None = None,
        account_id: UUID | None = None,
    ) -> SafetyDecision:
        return await self.final_pre_mutation_check(
            SafetyRequest(
                user_id=user_id,
                action=action,
                execution_mode=execution_mode,
                account_id=account_id,
                trade_id=trade_id,
                intent_id=intent_id,
                quantity=quantity,
                attributable_quantity=attributable_quantity,
                lots=lots,
                strategy_key=strategy_key,
                instrument=instrument,
            )
        )


__all__ = ["RiskSafetyService"]
