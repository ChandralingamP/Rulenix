from __future__ import annotations

import asyncio
import hashlib
import json
import os
import socket
import subprocess
import sys
import time
from collections.abc import Iterator
from contextlib import contextmanager
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

import asyncpg
import httpx
import pytest

ROOT = Path(__file__).parents[1]
RUST_BINARY = ROOT.parent / "backend" / "target" / "debug" / "rulenix-backend.exe"
DATABASE_URL = os.environ.get(
    "TEST_DATABASE_URL",
    "postgresql+asyncpg://rulenix:12345678@localhost:5432/rulenix_test_clear_trades",
)
ADMIN_ID = "00000000-0000-0000-0000-000000001001"
USER_ID = "00000000-0000-0000-0000-000000001002"
ADMIN_SESSION_ID = "00000000-0000-0000-0000-000000001011"
USER_SESSION_ID = "00000000-0000-0000-0000-000000001012"
ADMIN_TOKEN = "phase10-admin-session"
USER_TOKEN = "phase10-user-session"
CSRF_TOKEN = "phase10-csrf"


def test_all_http_contracts_have_exactly_one_evidence_classification() -> None:
    matrix = json.loads((ROOT / "tests" / "parity" / "api_contract_matrix.json").read_text(encoding="utf-8"))
    contracts = {f"{item['method']} {item['path']}" for item in matrix["http"]}
    groups = matrix["http_evidence_overrides"]
    assert set(groups) == {
        "EXECUTABLE_DIFFERENTIAL",
        "CONTRACT_FIXTURE_PARITY",
        "ENVIRONMENT_GATED",
        "MUTATION_GATED",
    }
    classified = [contract for values in groups.values() for contract in values]
    assert len(classified) == len(set(classified)) == len(contracts) == 50
    assert set(classified) == contracts


def _asyncpg_url() -> str:
    return DATABASE_URL.replace("postgresql+asyncpg://", "postgresql://", 1)


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


async def _seed_http_fixtures() -> None:
    connection = await asyncpg.connect(_asyncpg_url())
    try:
        for session_id in (ADMIN_SESSION_ID, USER_SESSION_ID):
            await connection.execute("DELETE FROM user_sessions WHERE id=$1", session_id)
        for user_id in (ADMIN_ID, USER_ID):
            await connection.execute("DELETE FROM risk_kill_switches WHERE user_id=$1", user_id)
            await connection.execute("DELETE FROM risk_limits WHERE user_id=$1", user_id)
            await connection.execute("DELETE FROM user_strategy_configs WHERE user_id=$1", user_id)
            await connection.execute("DELETE FROM user_strategy_activations WHERE user_id=$1", user_id)
            await connection.execute("DELETE FROM broker_reconciliation_health WHERE user_id=$1", user_id)
        await connection.execute("DELETE FROM risk_kill_switches WHERE user_id IS NULL")
        await connection.execute("DELETE FROM risk_limits WHERE user_id IS NULL")
        await connection.execute(
            """
            INSERT INTO users(
                id,username,email,password_hash,is_active,can_administer,can_live_trade,
                can_backtest,can_backtest_on_trading_days,password_changed_at
            ) VALUES
              ($1,'PHASE10ADMIN','phase10-admin@example.test','fixture',TRUE,TRUE,FALSE,TRUE,TRUE,NOW()-INTERVAL '1 day'),
              ($2,'PHASE10USER','phase10-user@example.test','fixture',TRUE,FALSE,FALSE,TRUE,FALSE,NOW()-INTERVAL '1 day')
            ON CONFLICT(id) DO UPDATE SET username=EXCLUDED.username,email=EXCLUDED.email,
              password_hash=EXCLUDED.password_hash,is_active=EXCLUDED.is_active,
              can_administer=EXCLUDED.can_administer,can_live_trade=EXCLUDED.can_live_trade,
              can_backtest=EXCLUDED.can_backtest,
              can_backtest_on_trading_days=EXCLUDED.can_backtest_on_trading_days,
              password_changed_at=EXCLUDED.password_changed_at
            """,
            ADMIN_ID,
            USER_ID,
        )
        await connection.execute(
            """
            INSERT INTO user_profiles(user_id,brokerage_user_id,api_key,trading_mode,token_state,last_token_status)
            VALUES($1,'ADMIN-BROKER','','demo','missing','missing'),($2,'USER-BROKER','','demo','missing','missing')
            ON CONFLICT(user_id) DO UPDATE SET brokerage_user_id=EXCLUDED.brokerage_user_id,
              api_key=EXCLUDED.api_key,trading_mode=EXCLUDED.trading_mode,
              token_state=EXCLUDED.token_state,last_token_status=EXCLUDED.last_token_status
            """,
            ADMIN_ID,
            USER_ID,
        )
        await connection.execute(
            "INSERT INTO risk_kill_switches(user_id,enabled,reason,updated_by) VALUES(NULL,FALSE,'',$1)",
            ADMIN_ID,
        )
        await connection.execute(
            "INSERT INTO risk_limits(user_id,updated_by) VALUES(NULL,$1)",
            ADMIN_ID,
        )
        csrf_hash = hashlib.sha256(CSRF_TOKEN.encode()).digest()
        for session_id, user_id, token in (
            (ADMIN_SESSION_ID, ADMIN_ID, ADMIN_TOKEN),
            (USER_SESSION_ID, USER_ID, USER_TOKEN),
        ):
            await connection.execute(
                """
                INSERT INTO user_sessions(
                    id,user_id,token_hash,csrf_hash,created_at,last_seen_at,
                    idle_expires_at,absolute_expires_at,user_agent,ip_address
                ) VALUES($1,$2,$3,$4,NOW(),NOW(),NOW()+INTERVAL '1 hour',NOW()+INTERVAL '2 hours','phase10','127.0.0.1')
                """,
                session_id,
                user_id,
                hashlib.sha256(token.encode()).digest(),
                csrf_hash,
            )
    finally:
        await connection.close()


@contextmanager
def _servers() -> Iterator[tuple[str, str]]:
    if not RUST_BINARY.exists():
        raise AssertionError("build the phase10 adapter before running HTTP differential tests")
    environment = {
        **os.environ,
        "RUST_LOG": "error",
        "TEST_DATABASE_URL": DATABASE_URL,
        "DATABASE_URL": DATABASE_URL,
        "APP_ENV": "test",
        "RULENIX_LOG_DIRECTORY": str(ROOT / "tests" / "parity" / "empty-logs"),
    }
    rust = subprocess.Popen(
        [str(RUST_BINARY), "--phase10-http-server"],
        cwd=ROOT.parent,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        env=environment,
    )
    python_port = _free_port()
    python = subprocess.Popen(
        [sys.executable, "-m", "uvicorn", "app.main:app", "--host", "127.0.0.1", "--port", str(python_port), "--log-level", "error"],
        cwd=ROOT,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        text=True,
        env=environment,
    )
    try:
        assert rust.stdout is not None
        ready = rust.stdout.readline().strip()
        assert ready.startswith("PHASE10_HTTP_READY http://127.0.0.1:"), ready
        rust_base = ready.removeprefix("PHASE10_HTTP_READY ")
        python_base = f"http://127.0.0.1:{python_port}"
        _wait_for_http(rust_base)
        _wait_for_http(python_base)
        yield rust_base, python_base
    finally:
        python.terminate()
        rust.terminate()
        python.wait(timeout=10)
        rust.wait(timeout=10)


def _headers(role: str, *, csrf: bool = False) -> dict[str, str]:
    token = ADMIN_TOKEN if role == "admin" else USER_TOKEN
    result = {"Cookie": f"rulenix_session={token}"}
    if csrf:
        result["X-CSRF-Token"] = CSRF_TOKEN
    return result


def _normalized_json(response: httpx.Response) -> Any:
    try:
        value = response.json()
    except ValueError:
        return {"raw": response.text}

    def visit(item: Any) -> Any:
        if isinstance(item, dict):
            return {
                key: visit(child)
                for key, child in item.items()
                if key not in {"created_at", "updated_at", "last_seen_at"}
            }
        if isinstance(item, list):
            return [visit(child) for child in item]
        if isinstance(item, str) and "T" in item:
            try:
                parsed = datetime.fromisoformat(item)
            except ValueError:
                return item
            if parsed.tzinfo is not None:
                return parsed.astimezone(UTC).isoformat()
        return item

    return visit(value)


def _compare(
    rust_base: str,
    python_base: str,
    method: str,
    path: str,
    *,
    role: str | None = None,
    csrf: bool = False,
    json_body: dict[str, Any] | None = None,
) -> None:
    headers = _headers(role, csrf=csrf) if role else {}
    rust_response = httpx.request(method, f"{rust_base}/api{path}", headers=headers, json=json_body, timeout=5)
    python_response = httpx.request(method, f"{python_base}/api{path}", headers=headers, json=json_body, timeout=5)
    assert rust_response.status_code == python_response.status_code, (method, path, rust_response.status_code, rust_response.text, python_response.status_code, python_response.text)
    rust_json = _normalized_json(rust_response)
    python_json = _normalized_json(python_response)
    if path == "/metrics":
        rust_age = rust_json.pop("market_feed_age_seconds")
        python_age = python_json.pop("market_feed_age_seconds")
        assert (rust_age is None) == (python_age is None)
        if rust_age is not None and python_age is not None:
            assert abs(float(rust_age) - float(python_age)) < 2
    assert rust_json == python_json, (method, path, rust_response.text, python_response.text)


@pytest.fixture(scope="module")
def runtime_servers() -> Iterator[tuple[str, str]]:
    asyncio.run(_seed_http_fixtures())
    with _servers() as values:
        yield values


@pytest.mark.parametrize("path", ["/health", "/health/live", "/health/ready"])
def test_public_health_runtime_differential(path: str, runtime_servers: tuple[str, str]) -> None:
    _compare(*runtime_servers, "GET", path)


@pytest.mark.parametrize(
    ("method", "path", "body"),
    [
        ("POST", "/auth/request-otp/", {"email": "invalid", "username": "bad name"}),
        ("POST", "/auth/signup/", {"username": "bad name", "user_id": "", "api_key": "", "mobile": "", "email": "invalid", "password": "x", "confirm_password": "x", "otp": "000000"}),
        ("POST", "/auth/login/", {"username": "missing", "password": "bad"}),
        ("POST", "/auth/password/request-reset/", {"email": "invalid"}),
        ("POST", "/auth/password/verify-otp/", {"email": "missing@example.test", "otp": "000000"}),
        ("POST", "/auth/password/reset/", {"email": "missing@example.test", "otp": "000000", "password": "x", "confirm_password": "y"}),
    ],
)
def test_public_auth_error_runtime_differential(method: str, path: str, body: dict[str, Any], runtime_servers: tuple[str, str]) -> None:
    _compare(*runtime_servers, method, path, json_body=body)


@pytest.mark.parametrize(
    ("path", "role"),
    [
        ("/metrics", "admin"),
        ("/auth/access/", "user"),
        ("/auth/admin/users/", "admin"),
        ("/auth/admin/trades/daily/?date=1900-01-01", "admin"),
        ("/admin/egress-ips", "admin"),
        ("/home/status/", "user"),
        ("/account/profile", "user"),
        ("/pnl", "user"),
        ("/pnl/export", "user"),
        ("/backtesting/runs", "user"),
        ("/backtesting/runs/00000000-0000-0000-0000-000000009999/export", "user"),
        ("/logs/files/", "user"),
        ("/logs/content/?filename=invalid.txt", "user"),
        ("/scheduler/jobs/", "admin"),
        ("/risk/admin", "admin"),
        ("/risk/admin/kill-switch", "admin"),
        ("/strategy/futures-breakout", "user"),
        ("/strategies/admin/executions?date=1900-01-01", "admin"),
    ],
)
def test_authenticated_read_runtime_differential(path: str, role: str, runtime_servers: tuple[str, str]) -> None:
    _compare(*runtime_servers, "GET", path, role=role)


@pytest.mark.parametrize(("method", "path"), [("GET", "/auth/access/"), ("GET", "/auth/admin/users/"), ("PUT", "/risk/admin/kill-switch")])
def test_unauthenticated_runtime_differential(method: str, path: str, runtime_servers: tuple[str, str]) -> None:
    _compare(*runtime_servers, method, path, json_body={"enabled": True})


def test_unauthorized_and_missing_csrf_runtime_differential(runtime_servers: tuple[str, str]) -> None:
    _compare(*runtime_servers, "GET", "/auth/admin/users/", role="user")
    _compare(*runtime_servers, "PUT", "/risk/admin/kill-switch", role="admin", json_body={"enabled": True, "reason": "phase10"})


def _compare_isolated_mutation(
    rust_base: str,
    python_base: str,
    method: str,
    path: str,
    *,
    role: str,
    json_body: dict[str, Any] | None = None,
) -> None:
    headers = _headers(role, csrf=True)
    asyncio.run(_seed_http_fixtures())
    rust_response = httpx.request(method, f"{rust_base}/api{path}", headers=headers, json=json_body, timeout=5)
    asyncio.run(_seed_http_fixtures())
    python_response = httpx.request(method, f"{python_base}/api{path}", headers=headers, json=json_body, timeout=5)
    asyncio.run(_seed_http_fixtures())
    assert rust_response.status_code == python_response.status_code, (
        method,
        path,
        rust_response.status_code,
        rust_response.text,
        python_response.status_code,
        python_response.text,
    )
    assert _normalized_json(rust_response) == _normalized_json(python_response), (
        method,
        path,
        rust_response.text,
        python_response.text,
    )


@pytest.mark.parametrize(
    ("method", "path", "role", "body"),
    [
        ("POST", "/auth/logout/", "user", None),
        ("PATCH", "/auth/admin/users/", "admin", {"username": "PHASE10ADMIN", "can_administer": False}),
        ("DELETE", "/auth/admin/users/", "admin", {"username": "PHASE10ADMIN"}),
        ("DELETE", "/auth/admin/users/trade-logs/", "admin", {"username": "PHASE10USER", "scope": "demo"}),
        ("POST", "/admin/egress-ips", "admin", {"ip_address": "127.0.0.1"}),
        ("POST", "/admin/egress-ips/00000000-0000-0000-0000-000000009999/verify", "admin", {}),
        ("PUT", f"/admin/users/{USER_ID}/angel-egress", "admin", {"egress_ip_id": None}),
        ("PATCH", "/home/profile/", "user", {"api_key": ""}),
        ("PATCH", "/account/profile", "user", {"otp": "x", "new_username": "x", "email": "invalid", "mobile_number": "x", "client_id": ""}),
        ("PUT", "/account/trading-mode", "user", {"mode": "demo"}),
        ("POST", "/pnl/trades/00000000-0000-0000-0000-000000009999/close", "user", {}),
        ("POST", "/scheduler/trigger/", "admin", {"job_key": "cleanup_otps"}),
        ("PUT", "/risk/admin/limits", "admin", {"max_lots": 2, "max_quantity": 100, "max_notional": 100000, "max_open_positions": 2, "max_trades_per_day": 10, "max_daily_realized_loss": 1000, "max_daily_unrealized_loss": 1000, "max_price_age_seconds": 30}),
        ("PUT", f"/risk/admin/limits/{USER_ID}", "admin", {"max_lots": 2}),
        ("PUT", "/risk/admin/kill-switch", "admin", {"enabled": True, "reason": "phase10"}),
        ("PUT", f"/risk/admin/kill-switch/{USER_ID}", "admin", {"enabled": True, "reason": "phase10"}),
        ("PUT", "/strategy/futures-breakout", "user", {"instrument": "GOLDTEN", "enabled": False, "lots": 0}),
        ("POST", "/strategies/admin/executions/retry", "admin", {"intent_id": "00000000-0000-0000-0000-000000009999"}),
        ("PUT", "/strategies/not-a-strategy/activation", "user", {"active": True}),
    ],
)
def test_isolated_local_mutation_runtime_differential(
    method: str,
    path: str,
    role: str,
    body: dict[str, Any] | None,
    runtime_servers: tuple[str, str],
) -> None:
    _compare_isolated_mutation(*runtime_servers, method, path, role=role, json_body=body)
