from fastapi import APIRouter, Depends, Request
from fastapi.encoders import jsonable_encoder
from fastapi.responses import JSONResponse
from sqlalchemy import text
from sqlalchemy.ext.asyncio import AsyncSession

from ..db import ping
from ..dependencies import Principal, admin_only, get_db
from ..strategy.runtime import SchedulerHealthSnapshot

router = APIRouter(tags=["health"])


def _scheduler_payload(snapshot: SchedulerHealthSnapshot) -> dict:
    if snapshot.stale:
        status = "stale"
    elif snapshot.leader:
        status = "advancing"
    else:
        status = "standby"
    return {
        "status": status,
        "leader": snapshot.leader,
        "last_advance_at": snapshot.last_advance_at,
        "last_successful_dispatch_at": snapshot.last_successful_dispatch_at,
        "dispatch_count": snapshot.dispatch_count,
        "error_count": snapshot.error_count,
    }


@router.get("/health")
@router.get("/health/live")
async def live() -> dict:
    return {"status": "ok", "service": "Rulenix Rust API"}


@router.get("/health/ready")
async def ready(request: Request):
    database_ready = await ping(request.app.state.engine)
    scheduler = request.app.state.scheduler_health.snapshot()
    runtime = getattr(request.app.state, "production_runtime", None)
    runtime_ready = True if runtime is None else runtime.ready()
    ready = database_ready and not scheduler.stale and runtime_ready
    payload = {
        "status": "ready" if ready else "unready",
        "checks": {
            "database": "ok" if database_ready else "unavailable",
            "strategy_scheduler": _scheduler_payload(scheduler),
            **({"python_runtime": runtime.status()} if runtime is not None else {}),
        },
    }
    return JSONResponse(jsonable_encoder(payload), status_code=200 if ready else 503)


@router.get("/metrics")
async def metrics(
    request: Request,
    _: Principal = Depends(admin_only),
    db: AsyncSession = Depends(get_db),
):
    async def scalar(sql: str, default=0):
        value = await db.scalar(text(sql))
        return default if value is None else value

    return {
        "active_sessions": int(await scalar("SELECT COUNT(*) FROM user_sessions WHERE revoked_at IS NULL AND idle_expires_at>NOW() AND absolute_expires_at>NOW()")),
        "market_feed_age_seconds": await db.scalar(text("SELECT EXTRACT(EPOCH FROM (NOW()-MAX(received_at)))::float8 FROM market_price_ticks")),
        "scheduler_runs_today": await scalar("SELECT COALESCE(jsonb_object_agg(status,total),'{}'::jsonb) FROM (SELECT status,COUNT(*) AS total FROM strategy_scheduler_runs WHERE trade_date=CURRENT_DATE GROUP BY status) counts", {}),
        "orders": await scalar("SELECT COALESCE(jsonb_object_agg(status,total),'{}'::jsonb) FROM (SELECT status,COUNT(*) AS total FROM strategy_orders GROUP BY status) counts", {}),
        "execution_intents_24h": await scalar("SELECT COALESCE(jsonb_object_agg(status,total),'{}'::jsonb) FROM (SELECT status,COUNT(*) AS total FROM strategy_execution_intents WHERE created_at>NOW()-INTERVAL '24 hours' GROUP BY status) counts", {}),
        "incomplete_signals_today": int(await scalar("SELECT COUNT(*) FROM strategy_signals WHERE (signal_at AT TIME ZONE 'Asia/Kolkata')::date=(NOW() AT TIME ZONE 'Asia/Kolkata')::date AND status IN ('dispatching','partial','failed')")),
        "risk_rejections_24h": int(await scalar("SELECT COUNT(*) FROM risk_decisions WHERE allowed=FALSE AND created_at>NOW()-INTERVAL '24 hours'")),
        "broker_errors_24h": int(await scalar("SELECT COUNT(*) FROM broker_order_events WHERE (event_type LIKE '%failed%' OR event_type LIKE '%error%') AND created_at>NOW()-INTERVAL '24 hours'")),
        "reconciliation_unhealthy": int(await scalar("SELECT COUNT(*) FROM broker_reconciliation_health WHERE healthy=FALSE OR checked_at<NOW()-INTERVAL '5 minutes'")),
        "strategy_scheduler": _scheduler_payload(
            request.app.state.scheduler_health.snapshot()
        ),
        **(
            {"python_runtime": request.app.state.production_runtime.status()}
            if getattr(request.app.state, "production_runtime", None) is not None
            else {}
        ),
    }
