"""Durable signal fan-out with Rust-compatible uniqueness semantics."""

from dataclasses import dataclass
from datetime import datetime
from decimal import Decimal
from uuid import UUID, uuid4

from sqlalchemy import text
from sqlalchemy.ext.asyncio import AsyncSession


@dataclass(frozen=True)
class PreparedIntent:
    user_id: UUID
    snapshot_id: UUID | None
    trade_id: UUID | None
    strategy_key: str
    instrument: str
    session_key: str
    action: str
    role: str
    side: str
    order_type: str
    lots: int
    quantity: int
    price: Decimal
    trigger_price: Decimal | None = None
    expires_at: datetime | None = None


class SignalRepository:
    def __init__(self, session: AsyncSession):
        self.session = session

    async def materialize(
        self,
        *,
        strategy_key: str,
        instrument: str,
        session_key: str,
        signal_at: datetime,
        signal_type: str,
        snapshot_id: UUID | None,
        payload: dict,
        intents: list[PreparedIntent],
    ) -> tuple[UUID, int]:
        signal_id = uuid4()
        row = (
            await self.session.execute(
                text("""
            INSERT INTO strategy_signals(id,strategy_key,instrument,session_key,signal_at,snapshot_id,signal_type,expected_users,payload)
            VALUES(:id,:strategy,:instrument,:session,:at,:snapshot,:type,:expected,CAST(:payload AS jsonb))
            ON CONFLICT(strategy_key,instrument,session_key,signal_type) DO NOTHING
            RETURNING id
        """),
                {
                    "id": signal_id,
                    "strategy": strategy_key,
                    "instrument": instrument,
                    "session": session_key,
                    "at": signal_at,
                    "snapshot": snapshot_id,
                    "type": signal_type,
                    "expected": len(intents),
                    "payload": __import__("json").dumps(payload),
                },
            )
        ).scalar()
        if row is None:
            signal_id = (
                await self.session.execute(
                    text(
                        "SELECT id FROM strategy_signals WHERE strategy_key=:strategy AND instrument=:instrument AND session_key=:session AND signal_type=:type"
                    ),
                    {
                        "strategy": strategy_key,
                        "instrument": instrument,
                        "session": session_key,
                        "type": signal_type,
                    },
                )
            ).scalar_one()
        inserted = 0
        for intent in intents:
            result = await self.session.execute(
                text("""
                INSERT INTO strategy_execution_intents(id,signal_id,user_id,snapshot_id,trade_id,strategy_key,instrument,session_key,action,role,side,order_type,lots,quantity,price,trigger_price,expires_at)
                VALUES(:id,:signal,:user,:snapshot,:trade,:strategy,:instrument,:session,:action,:role,:side,:order_type,:lots,:quantity,:price,:trigger,:expires)
                ON CONFLICT DO NOTHING
            """),
                {
                    "id": uuid4(),
                    "signal": signal_id,
                    "user": intent.user_id,
                    "snapshot": intent.snapshot_id,
                    "trade": intent.trade_id,
                    "strategy": intent.strategy_key,
                    "instrument": intent.instrument,
                    "session": intent.session_key,
                    "action": intent.action,
                    "role": intent.role,
                    "side": intent.side,
                    "order_type": intent.order_type,
                    "lots": intent.lots,
                    "quantity": intent.quantity,
                    "price": float(intent.price),
                    "trigger": float(intent.trigger_price)
                    if intent.trigger_price is not None
                    else None,
                    "expires": intent.expires_at,
                },
            )
            inserted += int(getattr(result, "rowcount", 0))
        return UUID(str(signal_id)), inserted


def prepare_square_off_intent(
    *,
    user_id: UUID,
    trade_id: UUID,
    snapshot_id: UUID,
    strategy_key: str,
    instrument: str,
    session_key: str,
    side: str,
    quantity: int,
    price: Decimal,
    expires_at: datetime | None = None,
) -> PreparedIntent:
    """Represent EOD as a durable risk-reducing action, never a local close."""
    if quantity <= 0:
        raise ValueError("Square-off quantity must be positive.")
    return PreparedIntent(
        user_id,
        snapshot_id,
        trade_id,
        strategy_key,
        instrument,
        session_key,
        "SQUARE_OFF",
        "EMERGENCY_CLOSE",
        side,
        "MARKET",
        1,
        quantity,
        price,
        None,
        expires_at,
    )


__all__ = ["PreparedIntent", "SignalRepository", "prepare_square_off_intent"]
