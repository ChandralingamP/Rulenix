from __future__ import annotations

from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from fastapi import APIRouter, Depends, Query

from ..config import get_settings
from ..dependencies import Principal, current_user
from ..errors import DomainError

router = APIRouter(prefix="/logs", tags=["logs"])


@router.get("/files/")
async def files(user: Principal = Depends(current_user)):
    root = Path(get_settings().log_directory)
    if not root.exists():
        return {"count": 0, "files": []}
    prefix = "".join(char for char in user.username if char.isalnum() or char in "_-").upper()
    paths = [path for path in root.glob("*.log") if path.is_file() and path.name.upper().startswith(prefix)]
    values: list[dict[str, Any]] = []
    for path in paths:
        stat = path.stat()
        modified = datetime.fromtimestamp(stat.st_mtime, timezone.utc)
        values.append({"filename": path.name, "username": prefix, "size": stat.st_size, "size_mb": round(stat.st_size / 1_048_576, 2), "modified": modified, "modified_display": modified.strftime("%Y-%m-%d %H:%M:%S")})
    values.sort(key=lambda item: item["modified"], reverse=True)
    return {"count": len(values), "files": values}


@router.get("/content/")
async def content(
    filename: str = Query(...),
    lines: int = Query(500),
    tail: bool = Query(True),
    since_session: bool = Query(False),
    user: Principal = Depends(current_user),
):
    prefix = "".join(char for char in user.username if char.isalnum() or char in "_-").upper()
    if "/" in filename or "\\" in filename or ".." in filename or not filename.upper().startswith(prefix):
        raise DomainError(400, "Invalid filename.")
    path = Path(get_settings().log_directory) / filename
    if not path.is_file():
        raise DomainError(404, "Log file not found.")
    raw = path.read_text(encoding="utf-8", errors="replace")
    values = raw.splitlines()
    count = min(max(lines, 1), 5000)
    if since_session:
        markers = [index for index, line in enumerate(values) if "MARKET DATA SESSION" in line or "BROKER SESSION" in line]
        selected = values[markers[-1] if markers else max(len(values) - count, 0) :]
    elif tail:
        selected = values[-count:]
    else:
        selected = values[:count]
    size = path.stat().st_size
    return {"filename": filename, "content": "\n".join(selected), "lines_returned": len(selected), "size": size, "size_mb": size / 1_048_576}
