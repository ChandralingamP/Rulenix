import logging

from app.runtime import lifecycle
from app.runtime.lifecycle import SkipReporter


def _lines(caplog) -> list[str]:
    return [r.getMessage() for r in caplog.records if "strategy_skip" in r.getMessage()]


def test_skip_reporter_logs_reason_changes_and_suppresses_repeats(caplog, monkeypatch):
    clock = {"now": 1000.0}
    monkeypatch.setattr(lifecycle, "monotonic", lambda: clock["now"])
    reporter = SkipReporter(repeat_seconds=300)
    with caplog.at_level(logging.INFO, logger=lifecycle.logger.name):
        assert reporter.report("s", "NIFTY", "stale_candles", latest="a")
        assert not reporter.report("s", "NIFTY", "stale_candles", latest="b")
        clock["now"] += 299
        assert not reporter.report("s", "NIFTY", "stale_candles")
        clock["now"] += 2
        assert reporter.report("s", "NIFTY", "stale_candles")
        assert reporter.report("s", "NIFTY", "no_flip")
        assert reporter.report("s", "SENSEX", "no_flip")  # separate key
    lines = _lines(caplog)
    assert len(lines) == 4
    assert "reason=stale_candles latest=a" in lines[0]


def test_skip_reporter_respects_log_level(caplog):
    reporter = SkipReporter()
    with caplog.at_level(logging.INFO, logger=lifecycle.logger.name):
        reporter.report("s", "NIFTY", "no_flip", level=logging.DEBUG)
    assert _lines(caplog) == []
