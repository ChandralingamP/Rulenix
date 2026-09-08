from __future__ import annotations

import asyncio

from fastapi import APIRouter, WebSocket, WebSocketDisconnect
from sqlalchemy import text

from ..security import digest

router = APIRouter(tags=["websocket"])


async def _principal(websocket: WebSocket):
    token = websocket.cookies.get("rulenix_session")
    if not token:
        return None
    async with websocket.app.state.session_factory() as db:
        return (await db.execute(text("SELECT u.id,u.username FROM user_sessions s JOIN users u ON u.id=s.user_id WHERE s.token_hash=:token AND s.revoked_at IS NULL AND s.idle_expires_at>NOW() AND s.absolute_expires_at>NOW() AND u.is_active"), {"token": digest(token)})).mappings().first()


@router.websocket("/ws/strategy")
async def strategy_socket(websocket: WebSocket):
    user = await _principal(websocket)
    if not user:
        await websocket.close(code=4401); return
    await websocket.accept()
    try:
        await websocket.send_json({"type": "connected"})
        while True:
            try:
                message = await asyncio.wait_for(websocket.receive_text(), timeout=20)
                if message:
                    await websocket.send_json({"type": "pong"})
            except TimeoutError:
                await websocket.send_json({"type": "heartbeat"})
    except (WebSocketDisconnect, RuntimeError):
        return


@router.websocket("/ws/market")
async def market_socket(websocket: WebSocket):
    user = await _principal(websocket)
    if not user:
        await websocket.close(code=4401); return
    tokens = [value.strip() for value in websocket.query_params.get("tokens", "").split(",") if value.strip()]
    if not tokens:
        await websocket.close(code=4400); return
    await websocket.accept()
    try:
        await websocket.send_json({"type": "connected", "provider": "Angel One SmartAPI"})
        while True:
            try:
                await asyncio.wait_for(websocket.receive_text(), timeout=20)
            except TimeoutError:
                await websocket.send_json({"type": "heartbeat"})
    except (WebSocketDisconnect, RuntimeError):
        return
