import json
import os
from pathlib import Path
from uuid import uuid4

import pytest
from sqlalchemy import text
from sqlalchemy.ext.asyncio import AsyncSession, create_async_engine

from app.api.strategy import _catalog


def test_contract_matrix_covers_rust_and_browser_surfaces_without_frontend_changes():
    matrix = json.loads(Path(__file__).with_name("parity").joinpath("api_contract_matrix.json").read_text())
    assert len(matrix["http"]) == 50
    assert len(matrix["websockets"]) == 2
    assert all(item["python"] for item in matrix["http"])
    assert matrix["react_source_changes"] == 0
    assert matrix["active_react_api_calls"] == matrix["python_compatible_react_calls"] == 35
    assert matrix["incompatible_react_calls"] == 0


@pytest.mark.asyncio
async def test_postgres_strategy_catalog_contract_is_user_scoped():
    value = os.environ.get("TEST_DATABASE_URL")
    if not value:
        pytest.skip("TEST_DATABASE_URL is not configured; no shared database is used")
    url = value.replace("postgresql://", "postgresql+asyncpg://", 1).replace("postgres://", "postgresql+asyncpg://", 1)
    engine = create_async_engine(url)
    user = uuid4()
    try:
        async with engine.begin() as conn:
            await conn.execute(text("INSERT INTO users(id,username,email,password_hash) VALUES(:id,:username,:email,'test')"), {"id": user, "username": f"phase9-{user.hex[:10]}", "email": f"phase9-{user.hex[:10]}@test.invalid"})
            await conn.execute(text("INSERT INTO user_strategy_activations(user_id,strategy_key,is_active) VALUES(:user,'futures_breakout_v3',TRUE)"), {"user": user})
        async with AsyncSession(engine) as session:
            catalog = await _catalog(session, str(user))
        assert {item["key"] for item in catalog} == {"futures_breakout_v3", "supertrend_index_options_v1"}
        assert next(item for item in catalog if item["key"] == "futures_breakout_v3")["active"] is True
    finally:
        async with engine.begin() as conn:
            await conn.execute(text("DELETE FROM users WHERE id=:id"), {"id": user})
        await engine.dispose()
