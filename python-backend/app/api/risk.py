from fastapi import APIRouter, Depends
from pydantic import BaseModel, ConfigDict, Field
from sqlalchemy import text
from sqlalchemy.ext.asyncio import AsyncSession

from ..dependencies import Principal, admin_only, get_db

router = APIRouter(prefix="/risk/admin", tags=["risk"])

class KillSwitch(BaseModel):
    model_config = ConfigDict(extra="forbid")

    enabled: bool
    reason: str = Field(default="", max_length=500)

class Limits(BaseModel):
    model_config = ConfigDict(extra="forbid")

    max_lots: int | None = Field(default=None, gt=0)
    max_quantity: int | None = Field(default=None, gt=0)
    max_notional: float | None = Field(default=None, gt=0)
    max_open_positions: int | None = Field(default=None, gt=0)
    max_trades_per_day: int | None = Field(default=None, gt=0)
    max_daily_realized_loss: float | None = Field(default=None, gt=0)
    max_daily_unrealized_loss: float | None = Field(default=None, gt=0)
    max_price_age_seconds: int | None = Field(default=None, gt=0, le=3600)

@router.get("")
async def risk_state(_: Principal = Depends(admin_only), db: AsyncSession = Depends(get_db)):
    limits = await db.scalar(text("SELECT to_jsonb(r) FROM risk_limits r WHERE user_id IS NULL"))
    kill = await db.scalar(text("SELECT jsonb_build_object('enabled',enabled,'reason',reason,'updated_at',updated_at) FROM risk_kill_switches WHERE user_id IS NULL"))
    users = (await db.execute(text("SELECT jsonb_build_object('id',u.id,'username',u.username,'limits',to_jsonb(l)-'user_id'-'updated_by','kill_switch',jsonb_build_object('enabled',COALESCE(k.enabled,FALSE),'reason',COALESCE(k.reason,''))) FROM users u LEFT JOIN risk_limits l ON l.user_id=u.id LEFT JOIN risk_kill_switches k ON k.user_id=u.id ORDER BY u.username"))).scalars().all()
    return {"global_limits": limits, "global_kill_switch": kill, "users": list(users)}

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
    return await db.scalar(text("SELECT jsonb_build_object('enabled',enabled,'reason',reason,'updated_at',updated_at,'updated_by',updated_by) FROM risk_kill_switches WHERE user_id IS NULL"))

@router.put("/limits")
async def put_limits(body: Limits, user: Principal = Depends(admin_only), db: AsyncSession = Depends(get_db)):
    values = body.model_dump()
    cols = ",".join(f"{k}=:{k}" for k in values)
    values["actor"] = user.id
    await db.execute(text(f"INSERT INTO risk_limits(user_id,{','.join(values for values in body.model_dump())},updated_by,updated_at) VALUES(NULL,{','.join(':'+k for k in body.model_dump())},:actor,NOW()) ON CONFLICT ((TRUE)) WHERE user_id IS NULL DO UPDATE SET {cols},updated_by=:actor,updated_at=NOW()"), values)
    await db.commit()
    return await risk_state(user, db)


@router.put("/limits/{user_id}")
async def put_user_limits(user_id: str, body: Limits, actor: Principal = Depends(admin_only), db: AsyncSession = Depends(get_db)):
    values = body.model_dump() | {"user_id": user_id, "actor": actor.id}
    await db.execute(text("""INSERT INTO risk_limits(user_id,max_lots,max_quantity,max_notional,max_open_positions,max_trades_per_day,max_daily_realized_loss,max_daily_unrealized_loss,max_price_age_seconds,updated_by)
      VALUES(:user_id,:max_lots,:max_quantity,:max_notional,:max_open_positions,:max_trades_per_day,:max_daily_realized_loss,:max_daily_unrealized_loss,:max_price_age_seconds,:actor)
      ON CONFLICT(user_id) WHERE user_id IS NOT NULL DO UPDATE SET max_lots=EXCLUDED.max_lots,max_quantity=EXCLUDED.max_quantity,max_notional=EXCLUDED.max_notional,max_open_positions=EXCLUDED.max_open_positions,max_trades_per_day=EXCLUDED.max_trades_per_day,max_daily_realized_loss=EXCLUDED.max_daily_realized_loss,max_daily_unrealized_loss=EXCLUDED.max_daily_unrealized_loss,max_price_age_seconds=EXCLUDED.max_price_age_seconds,updated_by=EXCLUDED.updated_by,updated_at=NOW()"""), values)
    await db.commit()
    return await risk_state(actor, db)


@router.put("/kill-switch/{user_id}")
async def put_user_kill(user_id: str, body: KillSwitch, actor: Principal = Depends(admin_only), db: AsyncSession = Depends(get_db)):
    await db.execute(text("INSERT INTO risk_kill_switches(user_id,enabled,reason,updated_by,updated_at) VALUES(:user,:enabled,:reason,:actor,NOW()) ON CONFLICT(user_id) WHERE user_id IS NOT NULL DO UPDATE SET enabled=EXCLUDED.enabled,reason=EXCLUDED.reason,updated_by=EXCLUDED.updated_by,updated_at=NOW()"), {"user": user_id, "enabled": body.enabled, "reason": body.reason.strip(), "actor": actor.id})
    await db.commit()
    return await risk_state(actor, db)

