from fastapi import APIRouter, Request
from fastapi.responses import JSONResponse

from ..db import ping

router = APIRouter(tags=["health"])


@router.get("/health")
@router.get("/health/live")
async def live() -> dict:
    return {"status": "ok", "service": "Rulenix Python Foundation API", "live_trading": False}


@router.get("/health/ready")
async def ready(request: Request):
    ok = await ping(request.app.state.engine)
    payload = {"status": "ready" if ok else "not_ready", "checks": {"database": "ok" if ok else "unavailable", "python_foundation": "ok"}, "capabilities": {"foundation": True, "live_trading": False, "reason": "Python broker/trading layer is deferred."}}
    return JSONResponse(payload, status_code=200 if ok else 503)
