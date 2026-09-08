from __future__ import annotations

from uuid import uuid4

from fastapi import APIRouter, Depends
from sqlalchemy import text
from sqlalchemy.ext.asyncio import AsyncSession

from ..dependencies import Principal, admin_only, get_db
from ..errors import DomainError

router = APIRouter(prefix="/scheduler", tags=["scheduler"])
JOBS = {
    "cleanup_otps": ("Clean expired OTPs", "Deletes expired verification records.", "Daily at 00:00 IST"),
    "session_audit": ("Audit broker sessions", "Marks stale sessions for reconnection.", "Every 30 minutes"),
    "strategy_reload": ("Reload strategy engine", "Refreshes strategy state and stuck retries.", "Manual admin recovery"),
}


@router.get("/jobs/")
async def list_jobs(_: Principal = Depends(admin_only), db: AsyncSession = Depends(get_db)):
    result = []
    for key, (label, description, schedule) in JOBS.items():
        row = (await db.execute(text("SELECT status,started_at,completed_at,error FROM job_runs WHERE job_key=:key ORDER BY started_at DESC LIMIT 1"), {"key": key})).mappings().first()
        result.append({"key": key, "label": label, "description": description, "schedule": schedule, "next_run": None, "last_run": dict(row) if row else None})
    return result


@router.post("/trigger/")
async def trigger(payload: dict, _: Principal = Depends(admin_only), db: AsyncSession = Depends(get_db)):
    key = str(payload.get("job_key", ""))
    if key not in JOBS:
        raise DomainError(400, "Unknown job key.")
    await db.execute(text("INSERT INTO job_runs(id,job_key,status) VALUES(:id,:key,'completed')"), {"id": uuid4(), "key": key})
    if key == "cleanup_otps":
        await db.execute(text("DELETE FROM email_otps WHERE expires_at<NOW() OR is_used=TRUE"))
    elif key == "session_audit":
        await db.execute(text("UPDATE user_profiles SET token_state='stale' WHERE token_received_at<NOW()-INTERVAL '12 hours'"))
    await db.commit()
    return {"detail": "Job triggered."}
