import asyncio
import json
import os
import sys

import httpx
import pytest
from sqlalchemy import text
from sqlalchemy.ext.asyncio import create_async_engine

from app.broker.angel.egress import (
    DatabaseEgressBinding,
    EgressSelection,
    _bound_http_client,
    binding_ipv4,
    validate_public_ipv4,
)
from app.broker.angel.helper import EgressHelperClient


def test_rust_public_validation_and_alias_are_stable():
    assert str(validate_public_ipv4("51.161.140.103")) == "51.161.140.103"
    assert str(binding_ipv4("51.161.140.103")).startswith("100.")
    for value in ("127.0.0.1", "10.0.0.1", "100.64.0.1", "192.0.2.1"):
        with pytest.raises(ValueError):
            validate_public_ipv4(value)


async def _source_request(client: httpx.AsyncClient) -> str:
    listener = await asyncio.start_server(lambda r, w: None, "127.0.0.1", 0)
    address = listener.sockets[0].getsockname()
    observed: asyncio.Future[str] = asyncio.get_running_loop().create_future()

    async def handler(reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
        peer = writer.get_extra_info("peername")
        if not observed.done():
            observed.set_result(peer[0])
        await reader.read(4096)
        writer.write(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
        await writer.drain()
        writer.close()
        await writer.wait_closed()

    listener.close()
    await listener.wait_closed()
    listener = await asyncio.start_server(handler, "127.0.0.1", address[1])
    try:
        await client.get(f"http://127.0.0.1:{address[1]}/")
        return await asyncio.wait_for(observed, 2)
    finally:
        listener.close()
        await listener.wait_closed()


@pytest.mark.asyncio
async def test_explicit_rest_client_binds_actual_tcp_source_and_default_does_not_hardcode():
    async with _bound_http_client(EgressSelection("127.0.0.2")) as explicit:
        assert await _source_request(explicit) == "127.0.0.2"
    async with httpx.AsyncClient() as default:
        assert await _source_request(default) == "127.0.0.1"


@pytest.mark.asyncio
async def test_concurrent_explicit_accounts_keep_source_addresses_isolated():
    async with _bound_http_client(EgressSelection("127.0.0.2")) as first, _bound_http_client(EgressSelection("127.0.0.3")) as second:
        assert await asyncio.gather(_source_request(first), _source_request(second)) == ["127.0.0.2", "127.0.0.3"]


@pytest.mark.skipif(sys.platform == "win32", reason="Unix helper protocol is Linux-only")
@pytest.mark.asyncio
async def test_helper_client_sends_only_typed_contract(tmp_path):
    path = tmp_path / "helper.sock"
    seen: list[dict] = []

    async def serve(reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
        seen.append(json.loads((await reader.readline()).decode()))
        writer.write(b'{"ok":true,"configured":true,"verified":true,"observed_ip":"51.161.140.103","message":"ok"}\n')
        await writer.drain()
        writer.close()

    server = await asyncio.start_unix_server(serve, str(path))
    try:
        result = await EgressHelperClient(str(path)).configure_and_verify("51.161.140.103")
    finally:
        server.close()
        await server.wait_closed()
    assert result.verified and seen == [{"operation": "configure_and_verify", "ip_address": "51.161.140.103"}]


@pytest.mark.asyncio
async def test_postgres_egress_schema_and_null_assignment_execute_against_isolated_db():
    url = os.environ.get("TEST_DATABASE_URL")
    if not url:
        pytest.skip("TEST_DATABASE_URL is not configured; no shared database is used")
    if url.startswith("postgresql://"):
        url = url.replace("postgresql://", "postgresql+asyncpg://", 1)
    elif url.startswith("postgres://"):
        url = url.replace("postgres://", "postgresql+asyncpg://", 1)
    engine = create_async_engine(url)
    try:
        async with engine.begin() as connection:
            assert (await connection.execute(text("SELECT to_regclass('public.broker_egress_ips')"))).scalar_one()
        async with engine.connect() as connection:
            # A UUID with no profile assignment must resolve to ordinary OS routing.
            from sqlalchemy.ext.asyncio import AsyncSession
            async with AsyncSession(bind=connection) as session:
                selection = await DatabaseEgressBinding(session).select("00000000-0000-0000-0000-000000000000")
                assert selection.source_ip is None
    finally:
        await engine.dispose()
