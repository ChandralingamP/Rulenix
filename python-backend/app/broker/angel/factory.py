from collections.abc import Callable

import httpx

from .client import AngelClient
from .egress import DefaultEgressBinding, EgressBinding, require_transport_binding
from .models import AccountContext
from .mutation_guard import MutationGuard


class AngelClientFactory:
    def __init__(self, base_url: str, ws_url: str, egress: EgressBinding | None = None, transport_factory: Callable[[object | None], httpx.AsyncClient] | None = None, audit: Callable[[dict[str, str]], None] | None = None):
        self.base_url = base_url
        self.ws_url = ws_url
        self.egress = egress or DefaultEgressBinding()
        self.transport_factory = transport_factory or (lambda _binding: httpx.AsyncClient())
        self.audit = audit

    async def create(self, account: AccountContext) -> AngelClient:
        selection = await self.egress.select(account.user_id)
        binding = require_transport_binding(selection, self.egress.http_client_factory())
        transport = self.transport_factory(binding)
        return AngelClient(self.base_url, self.ws_url, account, transport, self.egress, MutationGuard(self.audit))

