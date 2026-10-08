"""PostgreSQL-backed safety state loader and final gate."""

from __future__ import annotations

from uuid import uuid4

from sqlalchemy import text
from sqlalchemy.exc import SQLAlchemyError
from sqlalchemy.ext.asyncio import AsyncSession

from .domain import (
    ActionClass,
    ActionKind,
    ReasonCode,
    SafetyDecision,
    SafetyRequest,
    SafetyState,
    evaluate,
)

ACTIVE_ORDER_STATES = (
    "('pending','submitting','ambiguous','submitted','partially_filled','processing','cancelling')"
)


class SafetyRepository:
    """Loads fresh, account-scoped state. It never calls a broker."""

    def __init__(self, session: AsyncSession):
        self.session = session

    async def _state(self, request: SafetyRequest) -> SafetyState:
        user = (
            (
                await self.session.execute(
                    text("""
            SELECT u.is_active, u.can_live_trade, COALESCE(p.trading_mode,'demo') AS trading_mode,
                   COALESCE(p.last_token_status,'') AS token_status,
                   p.broker_credential_revision, p.broker_egress_ip_id
              FROM users u LEFT JOIN user_profiles p ON p.user_id=u.id WHERE u.id=:user
        """),
                    {"user": request.user_id},
                )
            )
            .mappings()
            .first()
        )
        if user is None:
            return SafetyState(internal_error="account not found")
        kills = (
            (
                await self.session.execute(
                    text("""
            SELECT COALESCE((SELECT enabled FROM risk_kill_switches WHERE user_id IS NULL),FALSE) AS global_kill,
                   COALESCE((SELECT enabled FROM risk_kill_switches WHERE user_id=:user),FALSE) AS user_kill
        """),
                    {"user": request.user_id},
                )
            )
            .mappings()
            .one()
        )
        health = (
            (
                await self.session.execute(
                    text("""
            SELECT healthy, checked_at, broker_credential_revision
              FROM broker_reconciliation_health WHERE user_id=:user
        """),
                    {"user": request.user_id},
                )
            )
            .mappings()
            .first()
        )

        explicit = user["broker_egress_ip_id"] is not None
        egress_ready = True
        if explicit:
            egress_ready = bool(
                await self.session.scalar(
                    text("""
                SELECT EXISTS(
                    SELECT 1 FROM broker_egress_ips
                     WHERE id=:id AND configuration_status='CONFIGURED' AND verification_status='VERIFIED'
                )
            """),
                    {"id": user["broker_egress_ip_id"]},
                )
            )

        blockers: list[str] = []
        if request.execution_mode == "live":
            row = (
                (
                    await self.session.execute(
                        text("""
                SELECT COUNT(*) FILTER (WHERE status IN ('open','operator_required') AND ownership_status<>'manual_external') AS incidents,
                       COUNT(*) FILTER (WHERE status='open' AND ownership_status='ambiguous') AS protection_incidents
                  FROM broker_position_incidents WHERE user_id=:user
            """),
                        {"user": request.user_id},
                    )
                )
                .mappings()
                .one()
            )
            if int(row["incidents"] or 0):
                blockers.append("broker_position_incident")
            if int(row["protection_incidents"] or 0):
                blockers.append("unattributed_broker_position")
            blocker = (
                await self.session.execute(
                    text("SELECT status FROM broker_reconciliation_blockers WHERE user_id=:user"),
                    {"user": request.user_id},
                )
            ).scalar()
            if blocker == "open":
                blockers.append("broker_reconciliation_blocker")
            safety = (
                (
                    await self.session.execute(
                        text("""
                SELECT COUNT(*) FILTER (WHERE t.execution_mode='live' AND t.status='open' AND t.safety_status IN ('PROTECTION_REQUIRED','PROTECTION_SUBMITTING','PROTECTION_UNCERTAIN','PROTECTION_FAILED','RECONCILIATION_REQUIRED')) AS protection,
                       COUNT(*) FILTER (WHERE o.execution_mode='live' AND o.status='ambiguous') AS ambiguous_orders,
                       COUNT(*) FILTER (WHERE o.execution_mode='live' AND o.status IN ('pending','submitting','submitted','partially_filled','processing','cancelling')) AS active_orders
                  FROM trades t FULL OUTER JOIN strategy_orders o ON o.user_id=t.user_id WHERE COALESCE(t.user_id,o.user_id)=:user
            """),
                        {"user": request.user_id},
                    )
                )
                .mappings()
                .one()
            )
            if int(safety["protection"] or 0):
                blockers.append("protection_incident")

        pending = False
        if request.trade_id is not None:
            pending = bool(
                await self.session.scalar(
                    text("""
                SELECT EXISTS(
                    SELECT 1 FROM strategy_execution_intents
                     WHERE user_id=:user AND trade_id=:trade
                       AND status IN ('pending','claimed','retry_wait','submitted')
                       AND (CAST(:intent AS uuid) IS NULL OR id<>CAST(:intent AS uuid))
                ) OR EXISTS(
                    SELECT 1 FROM strategy_reversal_intents
                     WHERE user_id=:user AND source_trade_id=:trade
                       AND status IN ('pending','processing','waiting','submitted','failed')
                ) OR EXISTS(
                    SELECT 1 FROM manual_trade_close_intents
                     WHERE user_id=:user AND trade_id=:trade
                       AND status <> 'completed'
                )
            """),
                    {"user": request.user_id, "intent": request.intent_id, "trade": request.trade_id},
                )
            )
        elif request.strategy_key and request.instrument:
            pending = bool(
                await self.session.scalar(
                    text("""
                SELECT EXISTS(
                    SELECT 1 FROM strategy_execution_intents
                     WHERE user_id=:user AND strategy_key=:strategy AND instrument=:instrument
                       AND (CAST(:role AS varchar) IS NULL OR role=:role)
                       AND (CAST(:session_key AS varchar) IS NULL OR session_key=:session_key)
                       AND status IN ('pending','claimed','retry_wait','submitted')
                       AND (CAST(:intent AS uuid) IS NULL OR id<>CAST(:intent AS uuid))
                )
            """),
                    {
                        "user": request.user_id,
                        "strategy": request.strategy_key,
                        "instrument": request.instrument,
                        "role": (getattr(request, "role", None) or "").strip() or None,
                        "session_key": (getattr(request, "session_key", None) or "").strip() or None,
                        "intent": request.intent_id,
                    },
                )
            )
        ambiguous = bool(
            await self.session.scalar(
                text("""
            SELECT EXISTS(SELECT 1 FROM strategy_orders WHERE user_id=:user AND status='ambiguous'
                          AND (CAST(:order AS uuid) IS NULL OR id<>CAST(:order AS uuid)))
        """),
                {"user": request.user_id, "order": request.order_id},
            )
        )

        duplicate = False
        if (
            request.action in {ActionKind.ENTRY, ActionKind.SL2_REVERSAL}
            and request.strategy_key
            and request.instrument
        ):
            duplicate = bool(
                await self.session.scalar(
                    text("""
                SELECT EXISTS(
                    SELECT 1 FROM trades t
                     WHERE t.user_id=:user AND t.status='open' AND t.execution_mode=:mode
                       AND t.strategy_key=:strategy AND t.instrument_label=:instrument
                ) OR EXISTS(
                    SELECT 1 FROM strategy_orders o JOIN strategy_market_snapshots s ON s.id=o.snapshot_id
                     WHERE o.user_id=:user AND o.execution_mode=:mode
                       AND (
                           CASE
                               WHEN CAST(:role AS varchar) IS NOT NULL THEN (
                                   o.role=:role AND (CAST(:session_key AS varchar) IS NULL OR o.session_key=:session_key)
                               )
                               ELSE o.role IN ('BUY_ENTRY','SELL_ENTRY')
                           END
                       )
                       AND (CAST(:order AS uuid) IS NULL OR o.id<>CAST(:order AS uuid))
                       AND o.status IN ('pending','submitting','ambiguous','submitted','partially_filled','processing','cancelling')
                       AND s.strategy_key=:strategy AND s.instrument=:instrument
                )
            """),
                    {
                        "user": request.user_id,
                        "mode": request.execution_mode,
                        "strategy": request.strategy_key,
                        "instrument": request.instrument,
                        "order": request.order_id,
                        "role": (getattr(request, "role", None) or "").strip() or None,
                        "session_key": (getattr(request, "session_key", None) or "").strip() or None,
                    },
                )
            )

        broker_contract_collision = False
        if (
            request.execution_mode == "live"
            and request.action in {ActionKind.ENTRY, ActionKind.SL2_REVERSAL}
            and request.exchange_segment
            and request.contract_token
            and user["broker_credential_revision"] is not None
        ):
            broker_contract_collision = bool(await self.session.scalar(text("""
                SELECT EXISTS(
                    SELECT 1 FROM broker_exposure_observations
                     WHERE user_id=:user
                       AND UPPER(exchange_segment)=UPPER(:exchange)
                       AND contract_token=:token
                       AND ownership_status IN ('manual_external','ambiguous')
                       AND broker_credential_revision=:revision
                       AND observed_at>=NOW()-INTERVAL '5 minutes'
                )
            """), {
                "user": request.user_id, "exchange": request.exchange_segment,
                "token": request.contract_token,
                "revision": user["broker_credential_revision"],
            }))

        limits_row = (
            (
                await self.session.execute(
                    text("""
            SELECT COALESCE(u.max_lots,g.max_lots) max_lots, COALESCE(u.max_quantity,g.max_quantity) max_quantity,
                   COALESCE(u.max_notional,g.max_notional) max_notional, COALESCE(u.max_open_positions,g.max_open_positions) max_open_positions
              FROM risk_limits g LEFT JOIN risk_limits u ON u.user_id=:user WHERE g.user_id IS NULL
        """),
                    {"user": request.user_id},
                )
            )
            .mappings()
            .first()
        )
        projected: dict[str, int | float] = {}
        if request.action in {ActionKind.ENTRY, ActionKind.SL2_REVERSAL}:
            exposure = (
                (
                    await self.session.execute(
                        text("""
                SELECT COALESCE(SUM(CASE WHEN status='open' THEN total_lots ELSE 0 END),0) +
                       COALESCE((SELECT SUM(lots) FROM strategy_orders WHERE user_id=:user AND role IN ('BUY_ENTRY','SELL_ENTRY') AND status IN ('pending','submitting','ambiguous','submitted','partially_filled','processing','cancelling') AND (CAST(:order AS uuid) IS NULL OR id<>CAST(:order AS uuid))),0) lots,
                       COALESCE(SUM(CASE WHEN status='open' THEN quantity ELSE 0 END),0) +
                       COALESCE((SELECT SUM(quantity) FROM strategy_orders WHERE user_id=:user AND role IN ('BUY_ENTRY','SELL_ENTRY') AND status IN ('pending','submitting','ambiguous','submitted','partially_filled','processing','cancelling') AND (CAST(:order AS uuid) IS NULL OR id<>CAST(:order AS uuid))),0) quantity,
                       COUNT(*) FILTER (WHERE status='open') positions
                  FROM trades WHERE user_id=:user
            """),
                        {"user": request.user_id, "order": request.order_id},
                    )
                )
                .mappings()
                .one()
            )
            projected = {
                "max_lots": int(exposure["lots"] or 0) + request.lots,
                "max_quantity": int(exposure["quantity"] or 0) + request.quantity,
                "max_open_positions": int(exposure["positions"] or 0) + 1,
            }
        limits = (
            {
                key: limits_row[key]
                for key in ("max_lots", "max_quantity", "max_notional", "max_open_positions")
            }
            if limits_row
            else {}
        )
        existing_quantity = 0
        trade_open = True
        owner_matches = True
        if request.order_id:
            order_owner = (
                await self.session.execute(
                    text("SELECT user_id FROM strategy_orders WHERE id=:order"),
                    {"order": request.order_id},
                )
            ).scalar()
            owner_matches = order_owner == request.user_id
        if request.trade_id:
            trade = (
                (
                    await self.session.execute(
                        text(
                            "SELECT user_id,status,quantity,remaining_lots FROM trades WHERE id=:trade"
                        ),
                        {"trade": request.trade_id},
                    )
                )
                .mappings()
                .first()
            )
            owner_matches = owner_matches and trade is not None and trade["user_id"] == request.user_id
            if trade:
                trade_open = trade["status"] == "open"
                existing_quantity = int(trade["quantity"] or 0)

        return SafetyState(
            global_kill=bool(kills["global_kill"]),
            user_kill=bool(kills["user_kill"]),
            user_active=bool(user["is_active"]),
            can_live_trade=bool(user["can_live_trade"]),
            trading_mode=str(user["trading_mode"]),
            token_status=str(user["token_status"]),
            credential_revision=user["broker_credential_revision"],
            reconciled=bool(health and health["healthy"]),
            reconciliation_revision=health["broker_credential_revision"] if health else None,
            reconciliation_checked_at=health["checked_at"] if health else None,
            explicit_egress=explicit,
            egress_ready=egress_ready,
            blockers=tuple(blockers),
            pending_intent=pending,
            ambiguous_mutation=ambiguous,
            duplicate_exposure=duplicate,
            broker_contract_collision=broker_contract_collision,
            existing_quantity=existing_quantity,
            limits=limits,
            projected=projected,
            owner_matches=owner_matches,
            trade_open=trade_open,
            broker_evidence=bool(health and health["healthy"]),
        )

    async def final_pre_mutation_check(self, request: SafetyRequest) -> SafetyDecision:
        """Fresh final gate; no caller-supplied approval can bypass it."""
        try:
            if self.session.in_transaction():
                await self.session.execute(
                    text("SELECT pg_advisory_xact_lock_shared(hashtext('rulenix:risk:global'))")
                )
                await self.session.execute(
                    text("SELECT pg_advisory_xact_lock(hashtextextended(:user,0))"),
                    {"user": str(request.user_id)},
                )
                state = await self._state(request)
                decision = evaluate(request, state)
                await self._record(request, decision)
                return decision
            else:
                async with self.session.begin():
                    await self.session.execute(
                        text("SELECT pg_advisory_xact_lock_shared(hashtext('rulenix:risk:global'))")
                    )
                    await self.session.execute(
                        text("SELECT pg_advisory_xact_lock(hashtextextended(:user,0))"),
                        {"user": str(request.user_id)},
                    )
                    state = await self._state(request)
                    decision = evaluate(request, state)
                    await self._record(request, decision)
                    return decision
        except SQLAlchemyError as exc:
            return SafetyDecision.block(
                ActionClass.INCREASE_EXPOSURE,
                ReasonCode.INTERNAL_ERROR,
                "Safety evaluation failed closed.",
                conditions=(type(exc).__name__,),
                user_id=request.user_id,
            )

    async def _record(self, request: SafetyRequest, decision: SafetyDecision) -> None:
        # risk_decisions is the existing Rust audit table; order_id is intentionally
        # nullable at the schema level and is only an audit correlation here.
        await self.session.execute(
            text("""
            INSERT INTO risk_decisions(id,user_id,order_id,execution_mode,order_role,allowed,reason_code,message,values)
            VALUES(:id,:user,:order,:mode,:role,:allowed,:code,:message,CAST(:values AS jsonb))
        """),
            {
                "id": uuid4(),
                "user": request.user_id,
                "order": request.order_id or request.intent_id or request.trade_id,
                "mode": request.execution_mode,
                "role": ActionKind(request.action).value,
                "allowed": decision.allowed,
                "code": decision.reason_code.value,
                "message": decision.human_reason,
                "values": "{}",
            },
        )


__all__ = ["SafetyRepository"]
