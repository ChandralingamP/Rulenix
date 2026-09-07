from dataclasses import dataclass
from enum import StrEnum
from typing import Any


class BrokerErrorCategory(StrEnum):
    AUTHENTICATION_EXPIRED = "authentication_expired"
    AUTHENTICATION_INVALID = "authentication_invalid"
    TIMEOUT = "timeout"
    TRANSPORT_FAILURE = "transport_failure"
    RATE_LIMITED = "rate_limited"
    MALFORMED_RESPONSE = "malformed_response"
    BROKER_REJECTED = "broker_rejected"
    ORDER_NOT_FOUND = "order_not_found"
    UNSUPPORTED = "unsupported"
    AMBIGUOUS = "ambiguous"
    UNKNOWN = "unknown"


@dataclass(frozen=True)
class BrokerError(Exception):
    category: BrokerErrorCategory
    message: str
    operation: str
    status_code: int | None = None
    code: str | None = None
    retry_after_seconds: int | None = None
    retryable: bool = False
    raw: Any = None
    diagnostic: str = ""

    def __str__(self) -> str:
        return self.message


class EgressBindingError(BrokerError):
    def __init__(self, operation: str, message: str):
        super().__init__(BrokerErrorCategory.TRANSPORT_FAILURE, message, operation, retryable=False)


class MutationBlockedError(BrokerError):
    def __init__(self, operation: str, user_id: str):
        super().__init__(
            BrokerErrorCategory.UNSUPPORTED,
            f"Python Angel mutation is disabled: {operation}.",
            operation,
            retryable=False,
            diagnostic=f"blocked_before_transport user_id={user_id}",
        )

