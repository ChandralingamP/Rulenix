"""Nifty 50 Equivalent Opponent Strategy calculations, parity pairing, and ladder execution."""

from __future__ import annotations

from dataclasses import dataclass
from datetime import date, datetime, time
from decimal import Decimal
from enum import StrEnum

from .common import (
    Candle,
)

STRATEGY_KEY = "nifty_opponent_v1"
NIFTY_STRIKE_STEP = Decimal(50)
SPOT_ABORT_DELTA = Decimal(150)
RETRACEMENT_BAND_POINTS = Decimal(5)
TIMEFRAME_MINUTES = 3
BENCHMARK_TIME = time(9, 30)
BENCHMARK_CLOSE_TIME = time(9, 33)
MAX_DAILY_TRADES_DEFAULT = 3
MAX_LOSSES_BEFORE_HALT = 2


class OptionSide(StrEnum):
    CALL = "CE"
    PUT = "PE"

    @property
    def opposite(self) -> OptionSide:
        return OptionSide.PUT if self is OptionSide.CALL else OptionSide.CALL


@dataclass(frozen=True)
class OptionContract:
    token: str
    symbol: str
    expiry: date
    lot_size: int
    strike: Decimal
    option_type: OptionSide
    premium: Decimal = Decimal(0)


@dataclass(frozen=True)
class OpponentReferenceLevels:
    """Projected trigger and failure boundaries for an option contract."""

    instrument_side: OptionSide
    marked_high: Decimal
    marked_low: Decimal


@dataclass(frozen=True)
class CoincidencePair:
    """A paired CE and PE contract combination ranked by premium proximity."""

    ce_contract: OptionContract
    pe_contract: OptionContract
    ce_candle: Candle
    pe_candle: Candle
    premium_diff: Decimal
    rank: int

    @property
    def ce_levels(self) -> OpponentReferenceLevels:
        # Mark PE's 09:30-09:33 High/Low onto CE
        return OpponentReferenceLevels(
            instrument_side=OptionSide.CALL,
            marked_high=self.pe_candle.high,
            marked_low=self.pe_candle.low,
        )

    @property
    def pe_levels(self) -> OpponentReferenceLevels:
        # Mark CE's 09:30-09:33 High/Low onto PE
        return OpponentReferenceLevels(
            instrument_side=OptionSide.PUT,
            marked_high=self.ce_candle.high,
            marked_low=self.ce_candle.low,
        )

    def levels_for(self, side: OptionSide) -> OpponentReferenceLevels:
        return self.ce_levels if side is OptionSide.CALL else self.pe_levels


@dataclass(frozen=True)
class OpponentTargetLadder:
    """Mathematical target ladder computed as fixed percentages of Marked High (H)."""

    marked_high: Decimal
    target_1: Decimal
    target_2: Decimal
    target_3: Decimal
    target_4: Decimal

    @classmethod
    def from_marked_high(cls, high: Decimal) -> OpponentTargetLadder:
        return cls(
            marked_high=high,
            target_1=high + (Decimal("0.25") * high),
            target_2=high + (Decimal("0.50") * high),
            target_3=high + (Decimal("0.75") * high),
            target_4=high + (Decimal("1.00") * high),
        )

    def trailing_sl_for_stage(
        self, stage: str, entry_price: Decimal, initial_sl: Decimal
    ) -> Decimal:
        """Return the appropriate trailing stop loss level based on ladder stage:
        - 'INITIAL': Initial SL (Marked Low)
        - 'T1_HIT': Move SL to Cost-to-Cost (entry_price)
        - 'T2_HIT': Trail SL to T1 Level
        - 'T3_HIT': Trail SL to T2 Level
        - 'T4_HIT': Trail SL to T3 Level
        """
        if stage == "T1_HIT":
            return entry_price
        elif stage == "T2_HIT":
            return self.target_1
        elif stage == "T3_HIT":
            return self.target_2
        elif stage == "T4_HIT":
            return self.target_3
        return initial_sl

    def next_target_for_stage(self, stage: str) -> Decimal | None:
        """Return the next target milestone for the given stage."""
        if stage == "INITIAL":
            return self.target_1
        elif stage == "T1_HIT":
            return self.target_2
        elif stage == "T2_HIT":
            return self.target_3
        elif stage == "T3_HIT":
            return self.target_4
        return None


@dataclass(frozen=True)
class OpponentSignal:
    """Triggered entry signal ready for retracement pullback execution."""

    side: OptionSide
    contract: OptionContract
    marked_high: Decimal
    marked_low: Decimal
    retracement_min: Decimal
    retracement_max: Decimal
    ladder: OpponentTargetLadder
    signal_at: datetime
    pair_rank: int


def benchmark_is_recording(now: datetime) -> bool:
    """Return True if within the 09:30:00 - 09:32:59 benchmark recording window."""
    t = now.time()
    return BENCHMARK_TIME <= t < BENCHMARK_CLOSE_TIME


def benchmark_is_due(now: datetime) -> bool:
    """Return True if 09:33:00 has arrived and the session is still within active trading hours."""
    t = now.time()
    return BENCHMARK_CLOSE_TIME <= t < time(15, 0)


def latest_completed_three_minute(now: datetime) -> datetime:
    """Return the timestamp of the latest closed 3-minute candle boundary."""
    minute = now.minute - (now.minute % TIMEFRAME_MINUTES)
    return now.replace(minute=minute, second=0, microsecond=0)


def calculate_atm_strike(spot_price: Decimal | str | float) -> Decimal:
    """Calculate the nearest At-The-Money strike for Nifty 50 (50 point grid)."""
    spot = Decimal(str(spot_price))
    remainder = spot % NIFTY_STRIKE_STEP
    if remainder >= NIFTY_STRIKE_STEP / 2:
        return spot + (NIFTY_STRIKE_STEP - remainder)
    return spot - remainder


def calculate_3_strike_perimeter(spot_price: Decimal | str | float) -> list[Decimal]:
    """Isolate tight 3-strike basket around spot: [ATM - 50, ATM, ATM + 50]."""
    atm = calculate_atm_strike(spot_price)
    return [atm - NIFTY_STRIKE_STEP, atm, atm + NIFTY_STRIKE_STEP]


def build_ranked_coincidence_queue(
    ce_contracts: dict[Decimal, tuple[OptionContract, Candle]],
    pe_contracts: dict[Decimal, tuple[OptionContract, Candle]],
    strikes: list[Decimal],
) -> list[CoincidencePair]:
    """Scan CE and PE contract pairs across the 3 strikes and rank by smallest |Close_CE - Close_PE|.

    Scans both identical-strike and cross-strike pairs within the 3-strike perimeter
    (e.g., 24500 CE with 24550 PE) to locate the pair whose option premiums coincide.
    """
    candidates: list[CoincidencePair] = []
    for ce_strike in strikes:
        if ce_strike not in ce_contracts:
            continue
        ce_contract, ce_candle = ce_contracts[ce_strike]
        for pe_strike in strikes:
            if pe_strike not in pe_contracts:
                continue
            pe_contract, pe_candle = pe_contracts[pe_strike]
            diff = abs(ce_candle.close - pe_candle.close)
            candidates.append(
                CoincidencePair(
                    ce_contract=ce_contract,
                    pe_contract=pe_contract,
                    ce_candle=ce_candle,
                    pe_candle=pe_candle,
                    premium_diff=diff,
                    rank=0,
                )
            )

    # Sort strictly by smallest absolute difference in closing premiums
    candidates.sort(key=lambda pair: pair.premium_diff)

    # Assign 1-based ranks
    ranked = [
        CoincidencePair(
            ce_contract=p.ce_contract,
            pe_contract=p.pe_contract,
            ce_candle=p.ce_candle,
            pe_candle=p.pe_candle,
            premium_diff=p.premium_diff,
            rank=idx + 1,
        )
        for idx, p in enumerate(candidates)
    ]
    return ranked


def verify_opponent_no_retouch(
    opponent_candles_after_breakdown: list[Candle],
    opponent_marked_low: Decimal,
) -> bool:
    """Verify Opponent 'No Re-Touch' rule.

    Following the opponent breakdown close, at least one subsequent candle must complete
    whose entire price action remains completely detached from the marked Low (High < Low).
    If any subsequent candle tests, wicks, or breaches the marked Low (High >= marked Low),
    the breakdown is invalidated and this function returns False.
    """
    if not opponent_candles_after_breakdown:
        return False

    for candle in opponent_candles_after_breakdown:
        if candle.high >= opponent_marked_low:
            return False

    return True


def check_confirmation_and_entry(
    pair: CoincidencePair,
    breakout_side: OptionSide,
    breakout_candles: list[Candle],
    opponent_candles: list[Candle],
) -> OpponentSignal | None:
    """Evaluate dual-directional split, opponent no-retouch filter, and arm entry."""
    breakout_levels = pair.levels_for(breakout_side)
    opponent_side = breakout_side.opposite
    opponent_levels = pair.levels_for(opponent_side)

    if not breakout_candles or not opponent_candles:
        return None

    # Step 1: Breakout side must demonstrate a decisive 3-minute candle close ABOVE marked High
    breakout_index: int | None = None
    for idx, c in enumerate(breakout_candles):
        if c.close > breakout_levels.marked_high:
            breakout_index = idx
            break

    if breakout_index is None:
        return None

    # Step 2: Opponent side must print a breakdown with a Red candle close BELOW marked Low
    # (Opponent candle must be Red: close < open, and close < marked_low)
    opponent_breakdown_index: int | None = None
    for idx, c in enumerate(opponent_candles):
        if c.close < c.open and c.close < opponent_levels.marked_low:
            opponent_breakdown_index = idx
            break

    if opponent_breakdown_index is None:
        return None

    # Step 3: Opponent "No Re-Touch" Rule
    # Subsequent candles after opponent breakdown close must form safely detached (High < marked_low)
    subsequent_opponent_candles = opponent_candles[opponent_breakdown_index + 1 :]
    if not verify_opponent_no_retouch(subsequent_opponent_candles, opponent_levels.marked_low):
        return None

    # Confirmation is valid! Construct execution zone: [Marked High, Marked High + 5 pts]
    contract = pair.ce_contract if breakout_side is OptionSide.CALL else pair.pe_contract
    marked_h = breakout_levels.marked_high
    marked_l = breakout_levels.marked_low
    ladder = OpponentTargetLadder.from_marked_high(marked_h)

    last_candle = breakout_candles[-1]
    return OpponentSignal(
        side=breakout_side,
        contract=contract,
        marked_high=marked_h,
        marked_low=marked_l,
        retracement_min=marked_h,
        retracement_max=marked_h + RETRACEMENT_BAND_POINTS,
        ladder=ladder,
        signal_at=last_candle.at,
        pair_rank=pair.rank,
    )


def is_in_retracement_zone(price: Decimal | str | float, marked_high: Decimal | str | float) -> bool:
    """Check if price is within the retracement zone: Marked High to (Marked High + 5 pts)."""
    p = Decimal(str(price))
    h = Decimal(str(marked_high))
    return h <= p <= h + RETRACEMENT_BAND_POINTS


def check_spot_expansion_abort(
    benchmark_spot: Decimal | str | float,
    current_spot: Decimal | str | float,
) -> bool:
    """Check if price launched > 150 points from 09:30 benchmark, voiding current levels."""
    bench = Decimal(str(benchmark_spot))
    curr = Decimal(str(current_spot))
    return abs(curr - bench) > SPOT_ABORT_DELTA


def calculate_partial_close_lots(
    total_lots: int,
    partial_pct: float | Decimal = 50,
) -> int:
    """Calculate partial close lot quantity from user-configured percentage (10% to 100%).

    Always closes at least 1 lot if partial_pct > 0 and total_lots > 1.
    """
    pct = Decimal(str(partial_pct)) / Decimal(100)
    pct = max(Decimal("0.10"), min(Decimal("1.00"), pct))
    calculated = int(Decimal(total_lots) * pct)
    if total_lots > 1:
        return max(1, min(total_lots - 1, calculated))
    return 1


def is_session_trading_allowed(
    total_trades_today: int,
    losses_today: int,
    max_daily_trades: int = MAX_DAILY_TRADES_DEFAULT,
) -> bool:
    """Enforce daily risk guardrails:

    - Maximum 3 trades/day during standard profitable conditions.
    - Loss circuit breaker: If any single trade logs a loss, limit session remainder
      to less than 2 trades (max 1 more attempt or halt).
    - If 2 losses occur, halt immediately.
    """
    if losses_today >= MAX_LOSSES_BEFORE_HALT:
        return False
    if losses_today == 1 and total_trades_today >= 2:
        return False
    return total_trades_today < max_daily_trades
