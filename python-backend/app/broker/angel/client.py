import httpx

from .auth import AngelAuthenticator
from .egress import EgressBinding
from .models import AccountContext
from .mutation_guard import MutationGuard
from .rest import AngelRestClient
from .retry import CooldownRegistry, RequestPacer
from .websocket import AngelWebSocketClient


class AngelClient:
    def __init__(self, base_url: str, ws_url: str, account: AccountContext, transport: httpx.AsyncClient, egress: EgressBinding, mutation_guard: MutationGuard):
        self.account = account
        self.rest = AngelRestClient(base_url, account, transport, RequestPacer(), CooldownRegistry())
        self.auth = AngelAuthenticator(base_url, transport)
        self.websocket = AngelWebSocketClient(ws_url, account, egress)
        self._mutation_guard = mutation_guard

    def place_order(self, *_args, **_kwargs) -> None:
        self._mutation_guard.block("place_order", self.account.user_id)

    def cancel_order(self, *_args, **_kwargs) -> None:
        self._mutation_guard.block("cancel_order", self.account.user_id)

    def manual_close(self, *_args, **_kwargs) -> None:
        self._mutation_guard.block("manual_close", self.account.user_id)

    async def close(self) -> None:
        await self.websocket.close()
        await self.rest.transport.aclose()
