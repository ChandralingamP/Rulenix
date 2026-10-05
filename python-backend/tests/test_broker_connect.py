from __future__ import annotations

from datetime import UTC, datetime, timedelta
from typing import Any
from uuid import uuid4

import httpx
import pytest
from sqlalchemy import text
from sqlalchemy.ext.asyncio import AsyncSession, async_sessionmaker

from app.broker.angel.credentials import CredentialCipher
from app.main import app, lifespan
from app.security import digest, encrypt_broker_secret, new_token


@pytest.fixture
async def app_client():
    async with lifespan(app):
        transport = httpx.ASGITransport(app=app)
        async with httpx.AsyncClient(transport=transport, base_url="http://test") as client:
            yield client


@pytest.fixture
async def test_user():
    user_id = str(uuid4())
    username = f"user_{user_id[:8]}"
    email = f"{username}@example.com"
    session_token = new_token()
    csrf_token = new_token()
    session_factory: async_sessionmaker[AsyncSession] = app.state.session_factory
    settings = app.state.settings

    client_id = f"CLIENT_{user_id[:6]}"
    api_key_plain = f"apikey_{user_id[:8]}"
    now = datetime.now(UTC)
    abs_exp = now + timedelta(hours=24)
    idle_exp = now + timedelta(minutes=30)

    async with session_factory() as db:
        await db.execute(
            text("""
            INSERT INTO users (id, username, email, password_hash, is_active, password_changed_at)
            VALUES (:id, :username, :email, 'test_hash', TRUE, NOW())
            """),
            {"id": user_id, "username": username, "email": email},
        )
        await db.execute(
            text("""
            INSERT INTO user_profiles (
                user_id, brokerage_user_id, token_state, last_token_status,
                last_token_message, trading_mode, broker_credential_revision
            )
            VALUES (:user_id, :client_id, 'idle', 'missing', '', 'demo', 1)
            """),
            {"user_id": user_id, "client_id": client_id},
        )
        await db.execute(
            text("""
            INSERT INTO user_sessions (
                id, user_id, token_hash, csrf_hash, absolute_expires_at, idle_expires_at, created_at
            )
            VALUES (
                :session_id, :user_id, :token_hash, :csrf_hash, :abs_exp, :idle_exp, NOW()
            )
            """),
            {
                "session_id": str(uuid4()),
                "user_id": user_id,
                "token_hash": digest(session_token),
                "csrf_hash": digest(csrf_token),
                "abs_exp": abs_exp,
                "idle_exp": idle_exp,
            },
        )

        version, nonce, ciphertext = encrypt_broker_secret(
            api_key_plain,
            user_id,
            "api_key",
            settings.credential_keys,
            settings.credential_primary_version,
        )
        await db.execute(
            text("""
            INSERT INTO broker_secrets (user_id, secret_kind, key_version, nonce, ciphertext)
            VALUES (:user_id, 'api_key', :version, :nonce, :ciphertext)
            """),
            {
                "user_id": user_id,
                "version": version,
                "nonce": nonce,
                "ciphertext": ciphertext,
            },
        )
        await db.commit()

    user_info = {
        "user_id": user_id,
        "username": username,
        "session_token": session_token,
        "csrf_token": csrf_token,
        "client_id": client_id,
        "api_key_plain": api_key_plain,
        "auth_headers": {
            "Cookie": f"rulenix_session={session_token}",
            "X-CSRF-Token": csrf_token,
        },
    }

    try:
        yield user_info
    finally:
        async with session_factory() as db:
            await db.execute(text("DELETE FROM users WHERE id = :id"), {"id": user_id})
            await db.commit()


@pytest.mark.asyncio
async def test_connect_missing_payload_deferred(app_client: httpx.AsyncClient):
    response = await app_client.post("/api/home/connect/")
    assert response.status_code == 503
    assert response.json()["code"] == "python_foundation_deferred"


@pytest.mark.asyncio
async def test_connect_unauthenticated_rejects(app_client: httpx.AsyncClient):
    response = await app_client.post(
        "/api/home/connect/",
        json={"mpin": "1234", "totp": "123456"},
    )
    assert response.status_code == 401
    assert "Authentication required" in response.json()["detail"]


@pytest.mark.asyncio
async def test_connect_invalid_session_rejects(app_client: httpx.AsyncClient):
    response = await app_client.post(
        "/api/home/connect/",
        json={"mpin": "1234", "totp": "123456"},
        headers={
            "Cookie": "rulenix_session=invalid-or-nonexistent-token",
            "X-CSRF-Token": "some-csrf",
        },
    )
    assert response.status_code == 401
    assert "Authentication required" in response.json()["detail"]


@pytest.mark.asyncio
async def test_connect_csrf_missing_or_invalid_rejects(
    app_client: httpx.AsyncClient, test_user: dict[str, Any]
):
    # Missing CSRF
    res1 = await app_client.post(
        "/api/home/connect/",
        json={"mpin": "1234", "totp": "123456"},
        headers={"Cookie": f"rulenix_session={test_user['session_token']}"},
    )
    assert res1.status_code == 403
    assert "Invalid CSRF token" in res1.json()["detail"]

    # Invalid CSRF
    res2 = await app_client.post(
        "/api/home/connect/",
        json={"mpin": "1234", "totp": "123456"},
        headers={
            "Cookie": f"rulenix_session={test_user['session_token']}",
            "X-CSRF-Token": "wrong-csrf-token",
        },
    )
    assert res2.status_code == 403
    assert "Invalid CSRF token" in res2.json()["detail"]


@pytest.mark.asyncio
async def test_connect_invalid_mpin_totp_rejects(
    app_client: httpx.AsyncClient, test_user: dict[str, Any]
):
    headers = test_user["auth_headers"]

    invalid_inputs = [
        {"mpin": "12", "totp": "123456"},  # mpin too short
        {"mpin": "12345678901234567", "totp": "123456"},  # mpin too long (>16)
        {"mpin": "abcd", "totp": "123456"},  # mpin non-numeric
        {"mpin": "1234", "totp": "123"},  # totp too short (<6)
        {"mpin": "1234", "totp": "123456789"},  # totp too long (>8)
        {"mpin": "1234", "totp": "abcdef"},  # totp non-numeric
    ]

    for payload in invalid_inputs:
        response = await app_client.post(
            "/api/home/connect/",
            json=payload,
            headers=headers,
        )
        assert response.status_code == 400
        assert "A valid MPIN and numeric TOTP are required." in response.json()["detail"]


@pytest.mark.asyncio
async def test_connect_missing_client_id_rejects(
    app_client: httpx.AsyncClient, test_user: dict[str, Any]
):
    session_factory: async_sessionmaker[AsyncSession] = app.state.session_factory
    async with session_factory() as db:
        await db.execute(
            text("UPDATE user_profiles SET brokerage_user_id = '' WHERE user_id = :id"),
            {"id": test_user["user_id"]},
        )
        await db.commit()

    response = await app_client.post(
        "/api/home/connect/",
        json={"mpin": "1234", "totp": "123456"},
        headers=test_user["auth_headers"],
    )
    assert response.status_code == 400
    assert "Add an Angel One Client ID before connecting." in response.json()["detail"]


@pytest.mark.asyncio
async def test_connect_missing_api_key_rejects(
    app_client: httpx.AsyncClient, test_user: dict[str, Any]
):
    session_factory: async_sessionmaker[AsyncSession] = app.state.session_factory
    async with session_factory() as db:
        await db.execute(
            text("DELETE FROM broker_secrets WHERE user_id = :id AND secret_kind = 'api_key'"),
            {"id": test_user["user_id"]},
        )
        await db.commit()

    response = await app_client.post(
        "/api/home/connect/",
        json={"mpin": "1234", "totp": "123456"},
        headers=test_user["auth_headers"],
    )
    assert response.status_code == 400
    assert "Add an Angel One API key before connecting." in response.json()["detail"]


@pytest.mark.asyncio
async def test_connect_egress_binding_failure(
    app_client: httpx.AsyncClient, test_user: dict[str, Any]
):
    session_factory: async_sessionmaker[AsyncSession] = app.state.session_factory
    egress_id = str(uuid4())
    async with session_factory() as db:
        await db.execute(
            text("""
            INSERT INTO broker_egress_ips (
                id, ip_address, configuration_status,
                verification_status, status_message, created_by
            )
            VALUES (
                :id, '198.51.100.55'::inet, 'CONFIGURATION_FAILED', 'VERIFICATION_FAILED',
                'Unreachable', :user_id
            )
            """),
            {"id": egress_id, "user_id": test_user["user_id"]},
        )
        await db.execute(
            text("UPDATE user_profiles SET broker_egress_ip_id = :egress_id WHERE user_id = :id"),
            {"egress_id": egress_id, "id": test_user["user_id"]},
        )
        await db.commit()

    try:
        response = await app_client.post(
            "/api/home/connect/",
            json={"mpin": "1234", "totp": "123456"},
            headers=test_user["auth_headers"],
        )
        assert response.status_code == 503
        assert response.json()["code"] == "egress_unavailable"
        assert "egress ip is unavailable" in response.json()["detail"].lower()
    finally:
        async with session_factory() as db:
            await db.execute(
                text("UPDATE user_profiles SET broker_egress_ip_id = NULL WHERE user_id = :id"),
                {"id": test_user["user_id"]},
            )
            await db.execute(
                text("DELETE FROM broker_egress_ips WHERE id = :id"),
                {"id": egress_id},
            )
            await db.commit()


@pytest.mark.asyncio
async def test_connect_broker_authentication_failure(
    app_client: httpx.AsyncClient, test_user: dict[str, Any]
):
    async def mock_handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(
            200,
            json={
                "status": False,
                "message": "Invalid MPIN or TOTP",
                "errorcode": "AB1004",
                "data": None,
            },
        )

    app.state.broker_transport_factory = lambda selection: httpx.AsyncClient(
        transport=httpx.MockTransport(mock_handler)
    )

    try:
        response = await app_client.post(
            "/api/home/connect/",
            json={"mpin": "1234", "totp": "123456"},
            headers=test_user["auth_headers"],
        )
        assert response.status_code == 400
        assert (
            "Broker connection failed. Verify the Client ID, API key, MPIN, and TOTP before retrying."
            in response.json()["detail"]
        )

        session_factory: async_sessionmaker[AsyncSession] = app.state.session_factory
        async with session_factory() as db:
            row = (
                (
                    await db.execute(
                        text(
                            "SELECT token_state, last_token_status FROM user_profiles WHERE user_id = :id"
                        ),
                        {"id": test_user["user_id"]},
                    )
                )
                .mappings()
                .first()
            )
            assert row is not None
            assert row["token_state"] == "failed"
            assert row["last_token_status"] == "failed"
    finally:
        app.state.broker_transport_factory = None


@pytest.mark.asyncio
async def test_connect_success_and_token_persistence(
    app_client: httpx.AsyncClient, test_user: dict[str, Any]
):
    expected_jwt = "test-jwt-token-alpha"
    expected_refresh = "test-refresh-token-beta"
    expected_feed = "test-feed-token-gamma"

    async def mock_handler(request: httpx.Request) -> httpx.Response:
        assert request.url.path.endswith("/loginByPassword")
        assert request.headers["x-privatekey"] == test_user["api_key_plain"]
        assert request.headers["x-usertype"] == "USER"
        import json
        req_body = json.loads(request.read())
        assert req_body["clientcode"] == test_user["client_id"]
        assert req_body["password"] == "1234"
        assert req_body["totp"] == "123456"
        assert req_body["password"] != "**********"
        assert req_body["totp"] != "**********"
        return httpx.Response(
            200,
            json={
                "status": True,
                "message": "SUCCESS",
                "errorcode": "",
                "data": {
                    "jwtToken": expected_jwt,
                    "refreshToken": expected_refresh,
                    "feedToken": expected_feed,
                },
            },
        )

    app.state.broker_transport_factory = lambda selection: httpx.AsyncClient(
        transport=httpx.MockTransport(mock_handler)
    )

    try:
        response = await app_client.post(
            "/api/home/connect/",
            json={"mpin": "1234", "totp": "123456"},
            headers=test_user["auth_headers"],
        )
        assert response.status_code == 200
        data = response.json()
        assert data["message"] == "Brokerage session established successfully."
        assert data["last_connected_at"] is not None
        assert data["details"]["client_id"] == test_user["client_id"]
        assert data["details"]["api_key_configured"] is True
        assert data["details"]["token_state"] == "connected"
        assert data["details"]["connection_state"] == "connected"
        assert data["details"]["connected_for_today"] is True

        session_factory: async_sessionmaker[AsyncSession] = app.state.session_factory
        async with session_factory() as db:
            p_row = (
                (
                    await db.execute(
                        text("""
                    SELECT token_state, last_token_status, last_token_message,
                           broker_credential_revision, token_received_at
                      FROM user_profiles WHERE user_id = :id
                    """),
                        {"id": test_user["user_id"]},
                    )
                )
                .mappings()
                .first()
            )
            assert p_row is not None
            assert p_row["token_state"] == "connected"
            assert p_row["last_token_status"] == "success"
            assert p_row["last_token_message"] == ""
            assert p_row["broker_credential_revision"] >= 2
            assert p_row["token_received_at"] is not None

            # Verify persisted encrypted tokens decrypt accurately
            settings = app.state.settings
            cipher = CredentialCipher(settings.credential_keys, settings.credential_primary_version)
            secrets_rows = (
                (
                    await db.execute(
                        text("""
                    SELECT secret_kind, key_version, nonce, ciphertext
                      FROM broker_secrets WHERE user_id = :id
                    """),
                        {"id": test_user["user_id"]},
                    )
                )
                .mappings()
                .all()
            )

            kinds = {r["secret_kind"]: r for r in secrets_rows}
            assert "jwt_token" in kinds
            assert "refresh_token" in kinds
            assert "feed_token" in kinds

            dec_jwt = cipher.decrypt(
                test_user["user_id"],
                "jwt_token",
                kinds["jwt_token"]["key_version"],
                bytes(kinds["jwt_token"]["nonce"]),
                bytes(kinds["jwt_token"]["ciphertext"]),
            )
            assert dec_jwt == expected_jwt

            dec_refresh = cipher.decrypt(
                test_user["user_id"],
                "refresh_token",
                kinds["refresh_token"]["key_version"],
                bytes(kinds["refresh_token"]["nonce"]),
                bytes(kinds["refresh_token"]["ciphertext"]),
            )
            assert dec_refresh == expected_refresh

            dec_feed = cipher.decrypt(
                test_user["user_id"],
                "feed_token",
                kinds["feed_token"]["key_version"],
                bytes(kinds["feed_token"]["nonce"]),
                bytes(kinds["feed_token"]["ciphertext"]),
            )
            assert dec_feed == expected_feed
    finally:
        app.state.broker_transport_factory = None


@pytest.mark.asyncio
async def test_connect_guarantees_no_broker_mutations_and_preserves_mode(
    app_client: httpx.AsyncClient, test_user: dict[str, Any]
):
    recorded_requests: list[httpx.Request] = []

    async def mock_handler(request: httpx.Request) -> httpx.Response:
        recorded_requests.append(request)
        return httpx.Response(
            200,
            json={
                "status": True,
                "message": "SUCCESS",
                "errorcode": "",
                "data": {
                    "jwtToken": "safe-jwt",
                    "refreshToken": "safe-refresh",
                    "feedToken": "safe-feed",
                },
            },
        )

    app.state.broker_transport_factory = lambda selection: httpx.AsyncClient(
        transport=httpx.MockTransport(mock_handler)
    )

    try:
        response = await app_client.post(
            "/api/home/connect/",
            json={"mpin": "1234", "totp": "123456"},
            headers=test_user["auth_headers"],
        )
        assert response.status_code == 200

        # Verify only login endpoint was accessed
        assert len(recorded_requests) == 1
        called_url = str(recorded_requests[0].url)
        assert "/loginByPassword" in called_url
        forbidden_substrings = [
            "placeOrder",
            "cancelOrder",
            "modifyOrder",
            "createRule",
            "modifyRule",
            "cancelRule",
        ]
        for sub in forbidden_substrings:
            assert sub not in called_url

        session_factory: async_sessionmaker[AsyncSession] = app.state.session_factory
        async with session_factory() as db:
            # Verify no order or trade mutations were recorded
            trades_count = (
                await db.execute(
                    text("SELECT COUNT(*) FROM trades WHERE user_id = :id"),
                    {"id": test_user["user_id"]},
                )
            ).scalar_one()
            assert trades_count == 0

            orders_count = (
                await db.execute(
                    text("SELECT COUNT(*) FROM strategy_orders WHERE user_id = :id"),
                    {"id": test_user["user_id"]},
                )
            ).scalar_one()
            assert orders_count == 0

            mutations_count = (
                await db.execute(
                    text("SELECT COUNT(*) FROM broker_mutation_attempts WHERE user_id = :id"),
                    {"id": test_user["user_id"]},
                )
            ).scalar_one()
            assert mutations_count == 0

            # Verify trading_mode was preserved as 'demo'
            mode = (
                await db.execute(
                    text("SELECT trading_mode FROM user_profiles WHERE user_id = :id"),
                    {"id": test_user["user_id"]},
                )
            ).scalar_one()
            assert mode == "demo"
    finally:
        app.state.broker_transport_factory = None


@pytest.mark.asyncio
async def test_status_expired_when_token_from_previous_day(
    app_client: httpx.AsyncClient, test_user: dict[str, Any]
):
    session_factory: async_sessionmaker[AsyncSession] = app.state.session_factory
    settings = app.state.settings
    user_id = test_user["user_id"]

    yesterday = datetime.now(UTC) - timedelta(days=1)
    async with session_factory() as db:
        for kind in ("jwt_token", "refresh_token", "feed_token"):
            version, nonce, ciphertext = encrypt_broker_secret(
                f"token_{kind}",
                user_id,
                kind,
                settings.credential_keys,
                settings.credential_primary_version,
            )
            await db.execute(
                text("""
                INSERT INTO broker_secrets (user_id, secret_kind, key_version, nonce, ciphertext)
                VALUES (:user_id, :kind, :version, :nonce, :ciphertext)
                ON CONFLICT (user_id, secret_kind) DO UPDATE
                  SET key_version = EXCLUDED.key_version, nonce = EXCLUDED.nonce, ciphertext = EXCLUDED.ciphertext
                """),
                {"user_id": user_id, "kind": kind, "version": version, "nonce": nonce, "ciphertext": ciphertext},
            )
        await db.execute(
            text("""
            UPDATE user_profiles
               SET token_state = 'connected',
                   last_token_status = 'success',
                   token_received_at = :yesterday,
                   last_token_check_at = :yesterday
             WHERE user_id = :user_id
            """),
            {"yesterday": yesterday, "user_id": user_id},
        )
        await db.commit()

    response = await app_client.get(
        "/api/home/status/",
        headers=test_user["auth_headers"],
    )
    assert response.status_code == 200
    data = response.json()
    assert data["connected_for_today"] is False
    assert data["connection_state"] == "expired"
    assert "Daily brokerage session expired" in data["connection_message"]

