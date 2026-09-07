from fastapi import APIRouter, Depends
from pydantic import BaseModel, Field
from sqlalchemy import text
from sqlalchemy.ext.asyncio import AsyncSession

from ..dependencies import Principal, admin_only, get_db

router = APIRouter(prefix="/risk/admin", tags=["risk"])

class KillSwitch(BaseModel):
    enabled: bool
    reason: str = Field(default="", max_length=500)

class Limits(BaseModel):
    max_lots: int | None = Field(default=None, gt=0)
    max_quantity: int | None = Field(default=None, gt=0)
    max_notional: float | None = Field(default=None, gt=0)
    max_open_positions: int | None = Field(default=None, gt=0)
    max_trades_per_day: int | None = Field(default=None, gt=0)
    max_daily_realized_loss: float | None = Field(default=None, gt=0)
    max_daily_unrealized_loss: float | None = Field(default=None, gt=0)
    max_price_age_seconds: int | None = Field(default=None, gt=0, le=3600)
    margin_requirement_percent: float | None = Field(default=None, ge=0, le=100)

@router.get("")
async def risk_state(_: Principal = Depends(admin_only), db: AsyncSession = Depends(get_db)):
    kill = (await db.execute(text("SELECT enabled,reason,updated_at,updated_by FROM risk_kill_switches WHERE user_id IS NULL"))).mappings().first()
    limits = (await db.execute(text("SELECT * FROM risk_limits WHERE user_id IS NULL"))).mappings().first()
    return {"global_kill_switch": dict(kill) if kill else {"enabled": False, "reason": ""}, "global_limits": dict(limits) if limits else {}}

@router.get("/kill-switch")
async def get_kill(_: Principal = Depends(admin_only), db: AsyncSession = Depends(get_db)):
    row = (await db.execute(text("SELECT enabled,reason,updated_at,updated_by FROM risk_kill_switches WHERE user_id IS NULL"))).mappings().first()
    return dict(row) if row else {"enabled": False, "reason": ""}

@router.put("/kill-switch")
async def put_kill(body: KillSwitch, user: Principal = Depends(admin_only), db: AsyncSession = Depends(get_db)):
    reason = body.reason.strip() or ("Emergency trading pause" if body.enabled else "Cleared by staff")
    async with db.begin():
        await db.execute(text("SELECT pg_advisory_xact_lock(hashtext('rulenix:risk:global'))"))
        await db.execute(text("""INSERT INTO risk_kill_switches(user_id,enabled,reason,updated_by,updated_at) VALUES(NULL,:enabled,:reason,:actor,NOW())
          ON CONFLICT ((TRUE)) WHERE user_id IS NULL DO UPDATE SET enabled=EXCLUDED.enabled,reason=EXCLUDED.reason,updated_by=EXCLUDED.updated_by,updated_at=NOW()"""), {"enabled": body.enabled, "reason": reason, "actor": user.id})
        await db.execute(text("INSERT INTO audit_events(event_type,actor_user_id,summary,metadata) VALUES('risk.global_kill_switch',:actor,:summary,CAST(:metadata AS jsonb))"), {"actor": user.id, "summary": "Global kill switch changed", "metadata": '{"trading_actions_deferred":true}'})
    return {"enabled": body.enabled, "reason": reason, "trading_actions_deferred": True}

@router.put("/limits")
async def put_limits(body: Limits, user: Principal = Depends(admin_only), db: AsyncSession = Depends(get_db)):
    values = body.model_dump()
    cols = ",".join(f"{k}=:{k}" for k in values)
    values["actor"] = user.id
    await db.execute(text(f"INSERT INTO risk_limits(user_id,{','.join(values for values in body.model_dump())},updated_by,updated_at) VALUES(NULL,{','.join(':'+k for k in body.model_dump())},:actor,NOW()) ON CONFLICT ((TRUE)) WHERE user_id IS NULL DO UPDATE SET {cols},updated_by=:actor,updated_at=NOW()"), values)
    await db.commit()
    return {"detail": "Risk limits updated."}

