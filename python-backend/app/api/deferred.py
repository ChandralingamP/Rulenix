from fastapi import APIRouter

from ..errors import DomainError

router = APIRouter(tags=["deferred"])

async def unavailable() -> None:
    raise DomainError(503, "Python foundation does not provide broker or trading capability yet.", code="python_foundation_deferred")

for _path, _method in [("/home/connect/", "post"), ("/home/profile/", "patch"), ("/pnl", "get"), ("/backtest", "post"), ("/strategies", "get"), ("/ws/strategy", "get")]:
    getattr(router, _method)(_path)(unavailable)

@router.get("/home/status/")
async def home_status():
    return {"status": "foundation_ready", "broker_connected": False, "live_trading": False, "message": "Broker layer is deferred."}

