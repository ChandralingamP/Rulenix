from __future__ import annotations

import os
import socket
import subprocess
import sys
import time
from pathlib import Path

import httpx

ROOT = Path(__file__).parents[1]
RUST_BINARY = ROOT.parent / "backend" / "target" / "debug" / "rulenix-backend.exe"


def _free_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


def _wait_for_http(base_url: str) -> None:
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        try:
            response = httpx.get(f"{base_url}/api/health", timeout=1)
            if response.status_code == 200:
                return
        except httpx.HTTPError:
            pass
        time.sleep(0.1)
    raise AssertionError(f"HTTP server did not become ready: {base_url}")


def test_safe_http_contracts_execute_against_rust_and_python_servers() -> None:
    if not RUST_BINARY.exists():
        raise AssertionError("build the phase10 adapter before running HTTP differential tests")
    rust = subprocess.Popen(
        [str(RUST_BINARY), "--phase10-http-server"],
        cwd=ROOT.parent,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        env={**os.environ, "RUST_LOG": "error"},
    )
    python_port = _free_port()
    python = subprocess.Popen(
        [sys.executable, "-m", "uvicorn", "app.main:app", "--host", "127.0.0.1", "--port", str(python_port), "--log-level", "error"],
        cwd=ROOT,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    try:
        assert rust.stdout is not None
        ready = rust.stdout.readline().strip()
        assert ready.startswith("PHASE10_HTTP_READY http://127.0.0.1:")
        rust_base = ready.removeprefix("PHASE10_HTTP_READY ")
        python_base = f"http://127.0.0.1:{python_port}"
        _wait_for_http(rust_base)
        _wait_for_http(python_base)
        for path in ("/api/health", "/api/health/live"):
            rust_response = httpx.get(f"{rust_base}{path}", timeout=2)
            python_response = httpx.get(f"{python_base}{path}", timeout=2)
            assert rust_response.status_code == python_response.status_code == 200
            assert rust_response.json() == python_response.json()
    finally:
        python.terminate()
        rust.terminate()
        python.wait(timeout=10)
        rust.wait(timeout=10)
