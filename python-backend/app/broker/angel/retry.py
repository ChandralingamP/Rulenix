import asyncio
import hashlib
import time
from collections import defaultdict, deque
from collections.abc import Awaitable, Callable
from dataclasses import dataclass
from typing import TypeVar

from .errors import BrokerError, BrokerErrorCategory

T = TypeVar("T")


@dataclass(frozen=True)
class RetryPlan:
    attempts: int
    delay_seconds: float


class CooldownRegistry:
    def __init__(self) -> None:
        self._until: dict[str, float] = {}
        self._lock = asyncio.Lock()

    async def remaining(self, key: str) -> int | None:
        async with self._lock:
            value = self._until.get(key)
            if value is None:
                return None
            remaining = int(max(0, value - time.monotonic()))
            if remaining <= 0:
                self._until.pop(key, None)
                return None
            return max(1, remaining)

    async def activate(self, key: str, seconds: int) -> None:
        async with self._lock:
            self._until[key] = max(self._until.get(key, 0), time.monotonic() + max(5, min(seconds, 300)))


class RequestPacer:
    """Per-account/path sliding-window pacing matching the Rust headroom."""

    def __init__(self) -> None:
        self._history: dict[str, deque[float]] = defaultdict(deque)
        self._lock = asyncio.Lock()

    async def acquire(self, account_key: str, operation: str, limits: tuple[tuple[int, float], ...]) -> None:
        key = f"{hashlib.sha256(account_key.encode()).hexdigest()}:{operation}"
        longest = max((window for _, window in limits), default=1.0)
        while True:
            async with self._lock:
                now = time.monotonic()
                history = self._history[key]
                while history and now - history[0] >= longest:
                    history.popleft()
                wait = 0.0
                for capacity, window in limits:
                    active = [at for at in history if now - at < window]
                    if len(active) >= capacity:
                        wait = max(wait, active[0] + window - now)
                if not wait:
                    history.append(now)
                    return
            await asyncio.sleep(wait + 0.01)


def retry_plan(operation: str) -> RetryPlan:
    if operation == "login":
        return RetryPlan(2, 0.35)
    if operation in {"candles", "refresh"}:
        return RetryPlan(2, 0.4)
    return RetryPlan(1, 0.0)


def retryable_read(error: BrokerError) -> bool:
    return error.category in {BrokerErrorCategory.TIMEOUT, BrokerErrorCategory.TRANSPORT_FAILURE} and error.retryable


async def bounded_retry[T](operation: str, call: Callable[[], Awaitable[T]], should_retry: Callable[[Exception], bool]) -> T:
    plan = retry_plan(operation)
    for attempt in range(plan.attempts):
        try:
            return await call()
        except Exception as error:
            if attempt + 1 >= plan.attempts or not should_retry(error):
                raise
            await asyncio.sleep(plan.delay_seconds)
    raise RuntimeError("unreachable")

