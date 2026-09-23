"""Isolated Angel read fixture used by the Phase 13B Linux shadow rehearsal."""

from __future__ import annotations

import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

MUTATION_SUFFIXES = (
    "/placeOrder",
    "/cancelOrder",
    "/modifyOrder",
    "/gtt/v1/createRule",
    "/gtt/v1/modifyRule",
    "/gtt/v1/cancelRule",
)
requests: list[dict[str, str]] = []


class Handler(BaseHTTPRequestHandler):
    def _send(self, status: int, value: object) -> None:
        payload = json.dumps(value).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def _handle(self) -> None:
        requests.append({"method": self.command, "path": self.path})
        if self.path == "/audit":
            mutations = [
                item for item in requests if item["path"].endswith(MUTATION_SUFFIXES)
            ]
            self._send(200, {"requests": requests, "mutations": mutations})
            return
        if self.path.endswith(MUTATION_SUFFIXES):
            self._send(500, {"status": False, "message": "mutation trap fired"})
            return
        self._send(200, {"status": True, "data": []})

    do_GET = _handle
    do_POST = _handle
    do_PUT = _handle
    do_DELETE = _handle

    def log_message(self, _format: str, *_args: object) -> None:
        return


if __name__ == "__main__":
    ThreadingHTTPServer(("0.0.0.0", 8081), Handler).serve_forever()
