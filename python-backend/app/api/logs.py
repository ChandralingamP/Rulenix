from __future__ import annotations

from pathlib import Path

from fastapi import APIRouter, Depends, Query

from ..config import get_settings
from ..dependencies import Principal, current_user
from ..errors import DomainError

router = APIRouter(prefix="/logs", tags=["logs"])


@router.get("/files/")
async def files(_: Principal = Depends(current_user)):
    root = Path(get_settings().log_directory)
    if not root.exists():
        return {"files": []}
    return {"files": sorted(path.name for path in root.glob("*.log") if path.is_file())}


@router.get("/content/")
async def content(name: str = Query(...), _: Principal = Depends(current_user)):
    if not name or Path(name).name != name or Path(name).suffix != ".log":
        raise DomainError(400, "A valid log file name is required.")
    path = Path(get_settings().log_directory) / name
    if not path.is_file():
        raise DomainError(404, "Log file not found.")
    return {"name": name, "content": path.read_text(encoding="utf-8", errors="replace")[-1_000_000:]}
