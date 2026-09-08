from __future__ import annotations

from fastapi import FastAPI, WebSocket

app = FastAPI()


def _authorized(websocket: WebSocket) -> bool:
    return websocket.cookies.get("rulenix_session") == "phase10-session"


@app.websocket("/api/ws/strategy")
async def strategy(websocket: WebSocket) -> None:
    if not _authorized(websocket):
        await websocket.accept()
        await websocket.close(code=4401)
        return
    await websocket.accept()
    await websocket.send_json({"type": "connected"})
    while True:
        message = await websocket.receive_text()
        await websocket.send_json({"type": "pong"}) if message else None


@app.websocket("/api/ws/market")
async def market(websocket: WebSocket) -> None:
    if not _authorized(websocket):
        await websocket.accept()
        await websocket.close(code=4401)
        return
    tokens = [value.strip() for value in websocket.query_params.get("tokens", "").split(",") if value.strip()]
    if not tokens:
        await websocket.accept()
        await websocket.close(code=4400)
        return
    await websocket.accept()
    await websocket.send_json({"type": "connected", "provider": "Angel One SmartAPI"})
    while True:
        await websocket.receive_text()
