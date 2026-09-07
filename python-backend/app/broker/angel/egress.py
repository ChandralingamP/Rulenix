from collections.abc import Callable
from dataclasses import dataclass
from ipaddress import IPv4Address
from typing import Any, Protocol

from sqlalchemy import text
from sqlalchemy.ext.asyncio import AsyncSession

from .errors import EgressBindingError

BROKEN_EGRESS_MESSAGE = "Configured Angel egress IP is unavailable; broker operation blocked."


def validate_public_ipv4(value: str) -> IPv4Address:
    try:
        ip = IPv4Address(value.strip())
    except ValueError as exc:
        raise ValueError("Enter a valid public IPv4 address.") from exc
    o = ip.packed
    invalid = (ip.is_unspecified or ip.is_loopback or ip.is_private or ip.is_link_local or ip.is_multicast
        or ip == IPv4Address("255.255.255.255") or o[0] == 0 or o[0] >= 240
        or (o[0] == 100 and 64 <= o[1] <= 127) or (o[0] == 169 and o[1] == 254)
        or (o[0] == 192 and o[1] == 0 and o[2] in (0, 2))
        or (o[0] == 198 and o[1] in (18, 19)) or (o[0] == 198 and o[1] == 51 and o[2] == 100)
        or (o[0] == 203 and o[1] == 0 and o[2] == 113))
    if invalid:
        raise ValueError("The Angel egress address must be globally routable public IPv4.")
    return ip


def binding_ipv4(public: IPv4Address | str) -> IPv4Address:
    raw = int(IPv4Address(public))
    slot = ((raw ^ (raw >> 22)) * 0x9E3779B1) & 0x003F_FFFF
    return IPv4Address((100 << 24) | ((64 + ((slot >> 16) & 0x3F)) << 16) | ((slot >> 8 & 0xFF) << 8) | (slot & 0xFF))


@dataclass(frozen=True)
class EgressSelection:
    source_ip: str | None
    public_ip: str | None = None

    @property
    def explicit(self) -> bool:
        return self.source_ip is not None


class EgressBinding(Protocol):
    async def select(self, user_id: str) -> EgressSelection: ...
    def http_client_factory(self) -> Callable[[EgressSelection], object] | None: ...


def _bound_http_client(selection: EgressSelection) -> Any:
    if not selection.source_ip:
        raise EgressBindingError("egress", "Explicit source selection is missing a bind address.")
    import httpx
    return httpx.AsyncClient(transport=httpx.AsyncHTTPTransport(local_address=selection.source_ip, trust_env=False))


class DefaultEgressBinding:
    async def select(self, user_id: str) -> EgressSelection:
        return EgressSelection(None)
    def http_client_factory(self) -> Callable[[EgressSelection], object] | None:
        return None


class StaticEgressBinding:
    def __init__(self, assignments: dict[str, str | None], client_factory: Callable[[EgressSelection], object] | None = None):
        self._assignments = dict(assignments)
        self._client_factory = client_factory
    async def select(self, user_id: str) -> EgressSelection:
        return EgressSelection(self._assignments.get(user_id))
    def http_client_factory(self) -> Callable[[EgressSelection], object] | None:
        return self._client_factory


class DatabaseEgressBinding:
    def __init__(self, session: AsyncSession):
        self.session = session
    async def select(self, user_id: str) -> EgressSelection:
        row = (await self.session.execute(text("""
            SELECT host(e.ip_address) AS ip_address, e.configuration_status, e.verification_status
              FROM user_profiles p JOIN broker_egress_ips e ON e.id=p.broker_egress_ip_id
             WHERE p.user_id=:user_id
        """), {"user_id": user_id})).mappings().first()
        if row is None:
            return EgressSelection(None)
        try:
            public = validate_public_ipv4(str(row["ip_address"]))
        except ValueError as exc:
            raise EgressBindingError("egress", BROKEN_EGRESS_MESSAGE) from exc
        if row["configuration_status"] != "CONFIGURED" or row["verification_status"] != "VERIFIED":
            raise EgressBindingError("egress", BROKEN_EGRESS_MESSAGE)
        return EgressSelection(str(binding_ipv4(public)), str(public))
    def http_client_factory(self) -> Callable[[EgressSelection], object]:
        return _bound_http_client


async def rehydrate_configured_ips(session: AsyncSession, helper: Any) -> None:
    """Re-establish configured aliases at startup; failures remain fail-closed."""
    rows = (await session.execute(text("SELECT id,host(ip_address) AS ip_address FROM broker_egress_ips WHERE configuration_status='CONFIGURED' ORDER BY created_at"))).mappings().all()
    for row in rows:
        try:
            response = await helper.configure_and_verify(row["ip_address"])
            if response.ok and response.configured and response.verified and response.observed_ip == row["ip_address"]:
                await session.execute(text("UPDATE broker_egress_ips SET last_verified_at=NOW(),updated_at=NOW(),status_message='' WHERE id=:id"), {"id": row["id"]})
            else:
                await session.execute(text("UPDATE broker_egress_ips SET verification_status='VERIFICATION_FAILED',status_message=:message,updated_at=NOW() WHERE id=:id"), {"id": row["id"], "message": str(response.message)[:512]})
        except Exception as exc:
            await session.execute(text("UPDATE broker_egress_ips SET configuration_status='CONFIGURATION_FAILED',verification_status='UNVERIFIED',status_message=:message,updated_at=NOW() WHERE id=:id"), {"id": row["id"], "message": str(exc)[:512]})
    await session.commit()


def require_transport_binding(selection: EgressSelection, factory: Callable[[EgressSelection], object] | None) -> object | None:
    if selection.explicit and factory is None:
        raise EgressBindingError("egress", f"Explicit source IP {selection.source_ip} cannot fall back to default networking.")
    return factory(selection) if factory else None
