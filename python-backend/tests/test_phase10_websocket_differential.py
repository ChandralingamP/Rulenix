from __future__ import annotations

import asyncio
import socket
import subprocess
import sys
import time
from pathlib import Path

import websockets

ROOT = Path(__file__).parents[1]
RUST_BINARY = ROOT.parent / "backend" / "target" / "debug" / "rulenix-backend.exe"


def _free_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


async def _wait_for_port(port: int) -> None:
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        try:
            _reader, writer = await asyncio.open_connection("127.0.0.1", port)
            writer.close()
            await writer.wait_closed()
            return
        except OSError:
            await asyncio.sleep(0.1)
    raise AssertionError(f"WebSocket server did not become ready on {port}")


async def _probe(base: str, path: str, query: str = "") -> tuple[dict, dict]:
    uri = f"ws://{base}{path}{query}"
    async with websockets.connect(uri, additional_headers={"Cookie": "rulenix_session=phase10-session"}) as socket:
        first = json_load(await socket.recv())
        if path.endswith("strategy"):
            await socket.send("ping")
            second = json_load(await socket.recv())
        else:
            second = first
        return first, second


def json_load(value: str | bytes) -> dict:
    import json

    return json.loads(value)


def test_both_browser_websockets_execute_against_rust_and_python_fixture_servers() -> None:
    if not RUST_BINARY.exists():
        raise AssertionError("build the phase10 adapter before running WebSocket differential tests")
    rust = subprocess.Popen([str(RUST_BINARY), "--phase10-http-server"], cwd=ROOT.parent, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    python_port = _free_port()
    python = subprocess.Popen([sys.executable, "-m", "uvicorn", "app.parity.ws_fixture_server:app", "--host", "127.0.0.1", "--port", str(python_port), "--log-level", "error"], cwd=ROOT, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    try:
        assert rust.stdout is not None
        ready = rust.stdout.readline().strip()
        rust_port = int(ready.rsplit(":", 1)[1])
        async def run() -> None:
            await _wait_for_port(rust_port)
            await _wait_for_port(python_port)
            for path, query in (("/api/ws/strategy", ""), ("/api/ws/market", "?tokens=26000,26009")):
                rust_result = await _probe(f"127.0.0.1:{rust_port}", path, query)
                python_result = await _probe(f"127.0.0.1:{python_port}", path, query)
                assert rust_result == python_result
        asyncio.run(run())
    finally:
        python.terminate()
        rust.terminate()
        python.wait(timeout=10)
        rust.wait(timeout=10)
