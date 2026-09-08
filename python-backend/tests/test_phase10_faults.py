import pytest

from app.parity.faults import FaultInjector


def test_fault_injection_is_deterministic_and_exhaustive():
    injector = FaultInjector()
    injector.fail_once("postgres.commit", RuntimeError("commit failed"))
    with pytest.raises(RuntimeError, match="commit failed"):
        injector.checkpoint("postgres.commit")
    injector.checkpoint("postgres.commit")
    assert injector.hits["postgres.commit"] == 2
    injector.assert_exhausted()

def test_independent_boundaries_do_not_consume_each_other():
    injector = FaultInjector()
    injector.fail_once("angel.positions", TimeoutError("positions timeout"))
    injector.fail_once("websocket.receive", ValueError("malformed frame"))
    with pytest.raises(TimeoutError):
        injector.checkpoint("angel.positions")
    with pytest.raises(ValueError):
        injector.checkpoint("websocket.receive")
    injector.assert_exhausted()
