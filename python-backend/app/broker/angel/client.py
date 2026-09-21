import httpx

from ..authority import AuthorityProof, AuthorityRuntime
from .auth import AngelAuthenticator
from .egress import EgressBinding
from .models import AccountContext, CancelOrderRequest, OrderMutationRequest
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

    @staticmethod
    def _valid_proof(proof: AuthorityProof | None, user_id: str) -> bool:
        return bool(
            proof
            and proof.runtime is AuthorityRuntime.PYTHON
            and str(proof.user_id) == user_id
        )

    async def place_order(
        self, request: OrderMutationRequest, *, proof: AuthorityProof | None = None
    ):
        if not self._valid_proof(proof, self.account.user_id):
            self._mutation_guard.block("place_order", self.account.user_id)
        return await self.rest.place_order(request)

    async def cancel_order(
        self, request: CancelOrderRequest, *, proof: AuthorityProof | None = None
    ):
        if not self._valid_proof(proof, self.account.user_id):
            self._mutation_guard.block("cancel_order", self.account.user_id)
        return await self.rest.cancel_order(request)

    async def manual_close(
        self, request: OrderMutationRequest, *, proof: AuthorityProof | None = None
    ):
        if not self._valid_proof(proof, self.account.user_id):
            self._mutation_guard.block("manual_close", self.account.user_id)
        return await self.rest.place_order(request)

    async def close(self) -> None:
        await self.websocket.close()
        await self.rest.transport.aclose()
