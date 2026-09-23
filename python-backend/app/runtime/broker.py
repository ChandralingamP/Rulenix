"""Production account/client construction with resolved per-account egress."""

from __future__ import annotations

from uuid import UUID

from sqlalchemy.ext.asyncio import AsyncSession, async_sessionmaker

from app.broker.angel.client import AngelClient
from app.broker.angel.credentials import CredentialCipher, PostgresCredentialProvider
from app.broker.angel.egress import DatabaseEgressBinding, StaticEgressBinding, _bound_http_client
from app.broker.angel.factory import AngelClientFactory
from app.config import Settings


class RuntimeBrokerClientFactory:
    def __init__(
        self,
        session_factory: async_sessionmaker[AsyncSession],
        settings: Settings,
    ) -> None:
        self.session_factory = session_factory
        self.settings = settings
        self.cipher = CredentialCipher(
            settings.credential_keys, settings.credential_primary_version
        )

    async def __call__(self, user_id: UUID) -> AngelClient:
        async with self.session_factory() as session:
            selection = await DatabaseEgressBinding(session).select(str(user_id))
            account = await PostgresCredentialProvider(session, self.cipher).load(
                str(user_id),
                client_local_ip=self.settings.angel_client_local_ip,
                client_public_ip=selection.public_ip
                or self.settings.angel_client_public_ip,
                client_mac_address=self.settings.angel_client_mac_address,
            )
        egress = StaticEgressBinding(
            {str(user_id): selection.source_ip},
            _bound_http_client if selection.explicit else None,
        )
        return await AngelClientFactory(
            self.settings.angel_base_url,
            self.settings.angel_websocket_url,
            egress,
        ).create(account)


__all__ = ["RuntimeBrokerClientFactory"]
