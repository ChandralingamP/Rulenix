from fastapi import APIRouter, Depends
from sqlalchemy import text
from sqlalchemy.ext.asyncio import AsyncSession

from ..dependencies import Principal, current_user, get_db
from ..errors import DomainError

router = APIRouter(prefix="/account", tags=["account"])

@router.get("/profile")
async def profile(user: Principal = Depends(current_user), db: AsyncSession = Depends(get_db)):
    row = (await db.execute(text("SELECT u.id,u.username,u.email,COALESCE(p.mobile_number,'') mobile_number,COALESCE(p.brokerage_user_id,'') brokerage_user_id,COALESCE(p.trading_mode,'demo') trading_mode FROM users u LEFT JOIN user_profiles p ON p.user_id=u.id WHERE u.id=:id"), {"id": user.id})).mappings().first()
    return dict(row) if row else {"username": user.username, "trading_mode": user.trading_mode}

@router.put("/trading-mode")
async def trading_mode(_: Principal = Depends(current_user)):
    raise DomainError(503, "Trading mode changes are deferred until the Python broker layer.", code="python_foundation_deferred")

