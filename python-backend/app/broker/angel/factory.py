from collections.abc import Callable

import httpx

from .client import AngelClient
from .egress import (
    DefaultEgressBinding,
    EgressBinding,
    EgressSelection,
    _bound_http_client,
    require_transport_binding,
)
from .models import AccountContext
from .mutation_guard import MutationGuard


class AngelClientFactory:
    def __init__(self, base_url: str, ws_url: str, egress: EgressBinding | None = None, transport_factory: Callable[[object | None], httpx.AsyncClient] | None = None, audit: Callable[[dict[str, str]], None] | None = None):
        self.base_url = base_url
        self.ws_url = ws_url
        self.egress = egress or DefaultEgressBinding()
        self.transport_factory = transport_factory or self._transport_for_selection
        self.audit = audit

    async def create(self, account: AccountContext) -> AngelClient:
        selection = await self.egress.select(account.user_id)
        capability = self.egress.http_client_factory()
        if selection.explicit and capability is None:
            require_transport_binding(selection, capability)
        # The selection is passed to the default transport so it can bind the
        # real socket; custom transports receive the same typed selection.
        binding = selection
        transport = self.transport_factory(binding)
        return AngelClient(self.base_url, self.ws_url, account, transport, self.egress, MutationGuard(self.audit))

    @staticmethod
    def _transport_for_selection(binding: object | None) -> httpx.AsyncClient:
        if isinstance(binding, EgressSelection) and binding.explicit:
            return _bound_http_client(binding)
        return httpx.AsyncClient(transport=httpx.AsyncHTTPTransport(trust_env=False))
