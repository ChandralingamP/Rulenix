import asyncio
import json
from dataclasses import dataclass
from ipaddress import IPv4Address

from .egress import validate_public_ipv4


@dataclass(frozen=True)
class HelperResponse:
    ok: bool
    configured: bool
    verified: bool
    observed_ip: str | None
    message: str


class EgressHelperClient:
    """Typed client for the existing restricted Unix-socket helper protocol."""

    def __init__(self, socket_path: str, timeout: float = 30.0, max_response: int = 8192):
        self.socket_path = socket_path
        self.timeout = timeout
        self.max_response = max_response

    async def configure_and_verify(self, ip_address: str | IPv4Address) -> HelperResponse:
        ip = validate_public_ipv4(str(ip_address))
        payload = (json.dumps({"operation": "configure_and_verify", "ip_address": str(ip)}, separators=(",", ":")) + "\n").encode()

        async def operation() -> HelperResponse:
            reader, writer = await asyncio.open_unix_connection(self.socket_path)  # type: ignore[attr-defined]
            try:
                writer.write(payload)
                await writer.drain()
                line = await reader.readline()
            finally:
                writer.close()
                await writer.wait_closed()
            if len(line) > self.max_response or not line:
                raise RuntimeError("restricted egress helper returned an invalid response size")
            raw = json.loads(line)
            if set(raw) != {"ok", "configured", "verified", "observed_ip", "message"}:
                raise RuntimeError("restricted egress helper returned an invalid typed response")
            observed = raw["observed_ip"]
            if observed is not None:
                observed = str(IPv4Address(observed))
            return HelperResponse(bool(raw["ok"]), bool(raw["configured"]), bool(raw["verified"]), observed, str(raw["message"])[:512])

        return await asyncio.wait_for(operation(), timeout=self.timeout)
