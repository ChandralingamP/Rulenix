import logging
from collections.abc import Callable

from .errors import MutationBlockedError

logger = logging.getLogger(__name__)


class MutationGuard:
    """Hard local boundary: no mutation method can reach the HTTP transport."""

    def __init__(self, audit: Callable[[dict[str, str]], None] | None = None):
        self._audit = audit

    def block(self, operation: str, user_id: str) -> None:
        event = {"event": "broker_mutation_blocked", "operation": operation, "user_id": user_id, "outcome": "blocked_before_transport"}
        logger.warning("broker mutation blocked operation=%s user_id=%s", operation, user_id)
        if self._audit:
            self._audit(event)
        raise MutationBlockedError(operation, user_id)

