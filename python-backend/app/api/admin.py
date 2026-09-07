from fastapi import APIRouter, Depends
from sqlalchemy import text
from sqlalchemy.ext.asyncio import AsyncSession

from ..dependencies import Principal, admin_only, get_db
from ..errors import DomainError

router = APIRouter(prefix="/auth/admin", tags=["admin"])

@router.get("/users/")
async def users(_: Principal = Depends(admin_only), db: AsyncSession = Depends(get_db)):
    rows = (await db.execute(text("""SELECT u.id,u.username,u.email,u.can_administer,u.can_live_trade,u.can_backtest,u.can_backtest_on_trading_days,COALESCE(p.trading_mode,'demo') trading_mode,u.is_active,u.created_at,p.brokerage_user_id,p.broker_egress_ip_id FROM users u LEFT JOIN user_profiles p ON p.user_id=u.id ORDER BY u.username"""))).mappings().all()
    return [dict(row) for row in rows]

@router.patch("/users/")
async def update_user(payload: dict, actor: Principal = Depends(admin_only), db: AsyncSession = Depends(get_db)):
    target = payload.get("user_id") or payload.get("id")
    if not target:
        raise DomainError(422, "user_id is required.")
    if str(target) == str(actor.id) and "can_administer" in payload and not payload["can_administer"]:
        raise DomainError(400, "You cannot remove your own administrator permission.")
    allowed = {k: payload[k] for k in ("can_administer", "can_live_trade", "can_backtest", "can_backtest_on_trading_days", "is_active") if k in payload}
    if not allowed:
        raise DomainError(422, "At least one permission is required.")
    sets = ",".join(f"{key}=:{key}" for key in allowed)
    allowed["id"] = target
    await db.execute(text(f"UPDATE users SET {sets},updated_at=NOW() WHERE id=:id"), allowed)
    await db.commit()
    return {"detail": "User updated."}

@router.get("/trade-logs/")
async def trade_logs(_: Principal = Depends(admin_only)):
    raise DomainError(503, "Trade log administration is deferred.", code="python_foundation_deferred")

