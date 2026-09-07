from collections.abc import Callable
from dataclasses import dataclass
from typing import Protocol

from .errors import EgressBindingError


@dataclass(frozen=True)
class EgressSelection:
    source_ip: str | None

    @property
    def explicit(self) -> bool:
        return self.source_ip is not None


class EgressBinding(Protocol):
    async def select(self, user_id: str) -> EgressSelection: ...

    def http_client_factory(self) -> Callable[[EgressSelection], object] | None: ...


class DefaultEgressBinding:
    async def select(self, user_id: str) -> EgressSelection:
        return EgressSelection(source_ip=None)

    def http_client_factory(self) -> Callable[[EgressSelection], object] | None:
        return None


class StaticEgressBinding:
    """Phase 3 interface; actual privileged alias/socket binding is Phase 4."""

    def __init__(self, assignments: dict[str, str | None], client_factory: Callable[[EgressSelection], object] | None = None):
        self._assignments = dict(assignments)
        self._client_factory = client_factory

    async def select(self, user_id: str) -> EgressSelection:
        return EgressSelection(source_ip=self._assignments.get(user_id))

    def http_client_factory(self) -> Callable[[EgressSelection], object] | None:
        return self._client_factory


def require_transport_binding(selection: EgressSelection, factory: Callable[[EgressSelection], object] | None) -> object | None:
    if selection.explicit and factory is None:
        raise EgressBindingError("egress", f"Explicit source IP {selection.source_ip} cannot fall back to default networking.")
    return factory(selection) if factory else None

