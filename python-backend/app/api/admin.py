from datetime import date as Date
from uuid import UUID

from fastapi import APIRouter, Depends, Query, Request
from sqlalchemy import text
from sqlalchemy.ext.asyncio import AsyncSession

from ..dependencies import Principal, admin_only, get_db
from ..errors import DomainError

router = APIRouter(prefix="/auth/admin", tags=["admin"])


@router.get("/users/")
async def users(_: Principal = Depends(admin_only), db: AsyncSession = Depends(get_db)):
    rows = (await db.execute(text("""SELECT u.id,u.username,u.email,u.can_administer,u.can_live_trade,u.can_backtest,u.can_backtest_on_trading_days,COALESCE(p.trading_mode,'demo') trading_mode,u.is_active,u.created_at,p.brokerage_user_id,p.broker_egress_ip_id,host(e.ip_address) AS broker_egress_ip,e.configuration_status AS broker_egress_configuration_status,e.verification_status AS broker_egress_verification_status FROM users u LEFT JOIN user_profiles p ON p.user_id=u.id LEFT JOIN broker_egress_ips e ON e.id=p.broker_egress_ip_id ORDER BY u.username"""))).mappings().all()
    return [dict(row) for row in rows]


@router.patch("/users/")
async def update_user(payload: dict, actor: Principal = Depends(admin_only), db: AsyncSession = Depends(get_db)):
    target = payload.get("user_id") or payload.get("id")
    if not target and payload.get("username"):
        target = await db.scalar(text("SELECT id FROM users WHERE LOWER(username)=LOWER(:username)"), {"username": payload["username"]})
    if not target:
        raise DomainError(422, "user_id is required.")
    if str(target) == str(actor.id) and "can_administer" in payload and not payload["can_administer"]:
        raise DomainError(400, "You cannot change your own administration permission.")
    allowed = {k: payload[k] for k in ("can_administer", "can_live_trade", "can_backtest", "can_backtest_on_trading_days", "is_active") if k in payload}
    if not allowed:
        raise DomainError(422, "At least one permission is required.")
    sets = ",".join(f"{key}=:{key}" for key in allowed)
    allowed["id"] = target
    await db.execute(text(f"UPDATE users SET {sets},updated_at=NOW() WHERE id=:id"), allowed)
    await db.commit()
    updated = (await db.execute(text("SELECT u.id,u.username,u.email,u.can_administer,u.can_live_trade,u.can_backtest,u.can_backtest_on_trading_days,u.is_active,COALESCE(p.trading_mode,'demo') trading_mode,p.brokerage_user_id,p.broker_egress_ip_id FROM users u LEFT JOIN user_profiles p ON p.user_id=u.id WHERE u.id=:id"), {"id": target})).mappings().first()
    return {"detail": "User updated.", "user": dict(updated) if updated else None}


@router.get("/trade-logs/")
async def trade_logs(_: Principal = Depends(admin_only)):
    raise DomainError(503, "Trade log administration is deferred.", code="python_foundation_deferred")


@router.delete("/users/")
async def delete_user(payload: dict, actor: Principal = Depends(admin_only), db: AsyncSession = Depends(get_db)):
    username = str(payload.get("username", "")).strip()
    target = (await db.execute(text("SELECT id,username FROM users WHERE LOWER(username)=LOWER(:username)"), {"username": username})).mappings().first()
    if not target:
        raise DomainError(404, "User was not found.")
    if str(target["id"]) == str(actor.id):
        raise DomainError(400, "You cannot delete your own account.")
    await db.execute(text("DELETE FROM users WHERE id=:id"), {"id": target["id"]})
    await db.commit()
    return {"detail": "User deleted."}


async def _verify_no_live_clear_blockers(db: AsyncSession, user_id: str) -> None:
    row = (await db.execute(
        text("""
        SELECT (SELECT COUNT(*) FROM trades WHERE user_id=:user AND execution_mode='live' AND status='open') AS open_live_trades,
               (SELECT COUNT(*) FROM trades WHERE user_id=:user AND execution_mode='live' AND status='closed' AND safety_status NOT IN ('CLOSED', 'DEMO')) AS unsafe_closed_trades,
               (SELECT COUNT(*) FROM strategy_orders WHERE user_id=:user AND execution_mode='live' AND status IN ('pending', 'submitted', 'open', 'trigger_pending', 'partially_filled', 'cancelling')) AS active_orders,
               (SELECT COUNT(*) FROM strategy_execution_intents WHERE user_id=:user AND status IN ('pending', 'claimed', 'retry_wait', 'submitted') AND (strategy_order_id IN (SELECT id FROM strategy_orders WHERE execution_mode='live') OR trade_id IN (SELECT id FROM trades WHERE execution_mode='live'))) AS execution_intents,
               (SELECT COUNT(*) FROM strategy_reversal_intents WHERE user_id=:user AND status IN ('pending', 'submitting')) AS reversals,
               (SELECT COUNT(*) FROM manual_trade_close_intents WHERE user_id=:user AND status IN ('pending', 'submitted')) AS manual_closes,
               (SELECT COUNT(*) FROM broker_position_incidents WHERE user_id=:user AND resolved_at IS NULL) AS broker_incidents,
               (SELECT COUNT(*) FROM broker_mutation_attempts WHERE user_id=:user AND terminal=FALSE) AS broker_mutations
        """),
        {"user": user_id},
    )).mappings().first()

    blockers = dict(row) if row else {}
    total = sum(int(v or 0) for v in blockers.values())
    if total > 0:
        details = ", ".join(f"{k}={v}" for k, v in blockers.items() if v)
        raise DomainError(
            400,
            f"Clear LIVE refused because durable LIVE exposure is unresolved ({details}). Reconcile or close it first.",
        )


async def _verify_broker_safe_for_live_clear(request: Request, db: AsyncSession, user_id: str) -> None:
    settings = getattr(request.app.state, "settings", None)
    session_factory = getattr(request.app.state, "session_factory", None)
    if not settings or not session_factory:
        return

    from ..runtime.broker import RuntimeBrokerClientFactory
    try:
        factory = RuntimeBrokerClientFactory(session_factory, settings)
        client = await factory(UUID(str(user_id)))
    except Exception as exc:
        raise DomainError(
            400,
            f"Clear LIVE requires a connected, readable Angel account: {exc}",
        ) from exc

    try:
        positions = await client.rest.positions()
    except Exception as exc:
        raise DomainError(
            400,
            f"Clear LIVE refused because Angel positions are unreadable; broker state is unknown: {exc}",
        ) from exc

    for pos in (positions or []):
        net_qty = int(getattr(pos, "net_quantity", 0) or 0)
        if net_qty != 0:
            sym = getattr(pos, "trading_symbol", "") or getattr(pos, "symbol", "") or "contract"
            raise DomainError(
                400,
                f"Clear LIVE refused because the Angel account has an open broker position ({sym} net_qty={net_qty}). Close it first.",
            )

    try:
        orders = await client.rest.order_book()
    except Exception as exc:
        raise DomainError(
            400,
            f"Clear LIVE refused because the Angel order book is unreadable; broker state is unknown: {exc}",
        ) from exc

    for ord_rec in (orders or []):
        status = str(getattr(ord_rec, "status", "")).strip().lower()
        if status in {"open", "trigger pending", "pending", "after market order req received", "modify after market order req received", "modify pending"}:
            ord_id = getattr(ord_rec, "order_id", "")
            raise DomainError(
                400,
                f"Clear LIVE refused because active Angel broker orders exist ({ord_id} status={status}). Cancel them first.",
            )

    try:
        rules = await client.rest.conditional_rules()
        for rule in (rules or []):
            rule_status = str(getattr(rule, "status", "")).strip().upper()
            if rule_status in {"ACTIVE", "PENDING", "NEW"}:
                raise DomainError(
                    400,
                    "Clear LIVE refused because active Angel conditional/GTT rules exist. Cancel them first.",
                )
    except DomainError:
        raise
    except Exception:
        pass


async def _clear_user_demo_trade_state(db: AsyncSession, user_id: str) -> dict[str, int]:
    reset_at = await db.scalar(
        text("UPDATE user_profiles SET demo_state_reset_at=clock_timestamp(), updated_at=NOW() WHERE user_id=:user RETURNING demo_state_reset_at"),
        {"user": user_id},
    )

    demo_trade_ids = [str(r[0]) for r in (await db.execute(
        text("SELECT id FROM trades WHERE user_id=:user AND execution_mode='demo' FOR UPDATE"),
        {"user": user_id},
    )).fetchall()]
    demo_order_ids = [str(r[0]) for r in (await db.execute(
        text("SELECT id FROM strategy_orders WHERE user_id=:user AND execution_mode='demo' FOR UPDATE"),
        {"user": user_id},
    )).fetchall()]

    candidate_snapshot_ids = [str(r[0]) for r in (await db.execute(
        text("""
        SELECT DISTINCT snapshot_id FROM strategy_orders WHERE user_id=:user AND execution_mode='demo'
        UNION
        SELECT DISTINCT strategy_snapshot_id FROM trades WHERE user_id=:user AND execution_mode='demo' AND strategy_snapshot_id IS NOT NULL
        """),
        {"user": user_id},
    )).fetchall() if r[0] is not None]

    removed_signals = [str(r[0]) for r in (await db.execute(
        text("""
        DELETE FROM strategy_execution_intents i
         USING strategy_signals s
         WHERE i.signal_id=s.id AND i.user_id=:user
           AND (
             i.strategy_order_id IN (SELECT id FROM strategy_orders WHERE user_id=:user AND execution_mode='demo')
             OR i.trade_id IN (SELECT id FROM trades WHERE user_id=:user AND execution_mode='demo')
           )
         RETURNING i.signal_id
        """),
        {"user": user_id},
    )).fetchall() if r[0] is not None]
    deleted_demo_intents = len(removed_signals)

    skipped_ambiguous = (await db.execute(
        text("""
        UPDATE strategy_execution_intents i
           SET status='skipped',
               last_error='Preserved and terminalized by Admin Clear Trades because execution mode was ambiguous.',
               completed_at=NOW(), updated_at=NOW()
          FROM strategy_signals s
         WHERE i.signal_id=s.id AND i.user_id=:user
           AND i.action='ENTRY'
           AND i.strategy_order_id IS NULL AND i.trade_id IS NULL
           AND i.status IN ('pending','claimed','retry_wait','submitted')
           AND s.signal_at <= :reset_at
        """),
        {"user": user_id, "reset_at": reset_at},
    )).rowcount or 0

    await db.execute(
        text("""
        DELETE FROM strategy_reversal_intents
         WHERE user_id=:user
           AND source_trade_id IN (SELECT id FROM trades WHERE user_id=:user AND execution_mode='demo')
        """),
        {"user": user_id},
    )

    if demo_trade_ids or demo_order_ids:
        deleted_demo_events = (await db.execute(
            text("""
            DELETE FROM strategy_events
             WHERE user_id=:user AND (
               payload->>'mode'='demo'
               OR payload->>'execution_mode'='demo'
               OR payload->>'trade_id' = ANY(CAST(:trades AS text[]))
               OR payload->>'order_id' = ANY(CAST(:orders AS text[]))
             )
            """),
            {"user": user_id, "trades": demo_trade_ids, "orders": demo_order_ids},
        )).rowcount or 0
    else:
        deleted_demo_events = (await db.execute(
            text("""
            DELETE FROM strategy_events
             WHERE user_id=:user AND (
               payload->>'mode'='demo'
               OR payload->>'execution_mode'='demo'
             )
            """),
            {"user": user_id},
        )).rowcount or 0

    deleted_demo_orders = (await db.execute(
        text("DELETE FROM strategy_orders WHERE user_id=:user AND execution_mode='demo'"),
        {"user": user_id},
    )).rowcount or 0

    deleted_demo_risk_decisions = (await db.execute(
        text("DELETE FROM risk_decisions WHERE user_id=:user AND execution_mode='demo'"),
        {"user": user_id},
    )).rowcount or 0

    deleted_demo_trades = (await db.execute(
        text("DELETE FROM trades WHERE user_id=:user AND execution_mode='demo'"),
        {"user": user_id},
    )).rowcount or 0

    deleted_orphan_signals = 0
    if removed_signals:
        deleted_orphan_signals = (await db.execute(
            text("""
            DELETE FROM strategy_signals s
             WHERE s.id = ANY(CAST(:signals AS uuid[]))
               AND NOT EXISTS (SELECT 1 FROM strategy_execution_intents i WHERE i.signal_id=s.id)
            """),
            {"signals": removed_signals},
        )).rowcount or 0

    deleted_orphan_snapshots = 0
    if candidate_snapshot_ids:
        deleted_orphan_snapshots = (await db.execute(
            text("""
            DELETE FROM strategy_market_snapshots s
             WHERE s.id = ANY(CAST(:snapshots AS uuid[]))
               AND NOT EXISTS (SELECT 1 FROM strategy_orders o WHERE o.snapshot_id=s.id)
               AND NOT EXISTS (SELECT 1 FROM trades t WHERE t.strategy_snapshot_id=s.id)
               AND NOT EXISTS (SELECT 1 FROM strategy_reversal_intents r WHERE r.snapshot_id=s.id)
               AND NOT EXISTS (SELECT 1 FROM strategy_signals sig WHERE sig.snapshot_id=s.id)
               AND NOT EXISTS (SELECT 1 FROM strategy_execution_intents i WHERE i.snapshot_id=s.id)
            """),
            {"snapshots": candidate_snapshot_ids},
        )).rowcount or 0

    return {
        "deleted_demo_trades": deleted_demo_trades,
        "deleted_demo_orders": deleted_demo_orders,
        "deleted_demo_intents": deleted_demo_intents,
        "skipped_ambiguous_intents": skipped_ambiguous,
        "deleted_demo_events": deleted_demo_events,
        "deleted_demo_risk_decisions": deleted_demo_risk_decisions,
        "deleted_orphan_signals": deleted_orphan_signals,
        "deleted_orphan_snapshots": deleted_orphan_snapshots,
    }


async def _clear_user_live_trade_state(db: AsyncSession, user_id: str) -> dict[str, int]:
    live_trade_ids = [str(r[0]) for r in (await db.execute(
        text("SELECT id FROM trades WHERE user_id=:user AND execution_mode='live' FOR UPDATE"),
        {"user": user_id},
    )).fetchall()]
    live_order_ids = [str(r[0]) for r in (await db.execute(
        text("SELECT id FROM strategy_orders WHERE user_id=:user AND execution_mode='live' FOR UPDATE"),
        {"user": user_id},
    )).fetchall()]

    candidate_snapshot_ids = [str(r[0]) for r in (await db.execute(
        text("""
        SELECT DISTINCT snapshot_id FROM strategy_orders WHERE user_id=:user AND execution_mode='live'
        UNION
        SELECT DISTINCT strategy_snapshot_id FROM trades WHERE user_id=:user AND execution_mode='live' AND strategy_snapshot_id IS NOT NULL
        """),
        {"user": user_id},
    )).fetchall() if r[0] is not None]

    removed_signals = [str(r[0]) for r in (await db.execute(
        text("""
        DELETE FROM strategy_execution_intents i
         WHERE i.user_id=:user
           AND (
             i.strategy_order_id IN (SELECT id FROM strategy_orders WHERE user_id=:user AND execution_mode='live')
             OR i.trade_id IN (SELECT id FROM trades WHERE user_id=:user AND execution_mode='live')
           )
         RETURNING i.signal_id
        """),
        {"user": user_id},
    )).fetchall() if r[0] is not None]
    deleted_live_intents = len(removed_signals)

    await db.execute(
        text("""
        DELETE FROM strategy_reversal_intents
         WHERE user_id=:user
           AND source_trade_id IN (SELECT id FROM trades WHERE user_id=:user AND execution_mode='live')
        """),
        {"user": user_id},
    )

    await db.execute(
        text("""
        DELETE FROM manual_trade_close_intents
         WHERE user_id=:user
           AND trade_id IN (SELECT id FROM trades WHERE user_id=:user AND execution_mode='live')
        """),
        {"user": user_id},
    )

    if live_trade_ids or live_order_ids:
        deleted_live_events = (await db.execute(
            text("""
            DELETE FROM strategy_events
             WHERE user_id=:user AND (
               payload->>'mode'='live'
               OR payload->>'execution_mode'='live'
               OR payload->>'trade_id' = ANY(CAST(:trades AS text[]))
               OR payload->>'order_id' = ANY(CAST(:orders AS text[]))
             )
            """),
            {"user": user_id, "trades": live_trade_ids, "orders": live_order_ids},
        )).rowcount or 0
    else:
        deleted_live_events = (await db.execute(
            text("""
            DELETE FROM strategy_events
             WHERE user_id=:user AND (
               payload->>'mode'='live'
               OR payload->>'execution_mode'='live'
             )
            """),
            {"user": user_id},
        )).rowcount or 0

    deleted_live_orders = (await db.execute(
        text("DELETE FROM strategy_orders WHERE user_id=:user AND execution_mode='live'"),
        {"user": user_id},
    )).rowcount or 0

    deleted_live_risk_decisions = (await db.execute(
        text("DELETE FROM risk_decisions WHERE user_id=:user AND execution_mode='live'"),
        {"user": user_id},
    )).rowcount or 0

    deleted_live_trades = (await db.execute(
        text("DELETE FROM trades WHERE user_id=:user AND execution_mode='live'"),
        {"user": user_id},
    )).rowcount or 0

    deleted_orphan_signals = 0
    if removed_signals:
        deleted_orphan_signals = (await db.execute(
            text("""
            DELETE FROM strategy_signals s
             WHERE s.id = ANY(CAST(:signals AS uuid[]))
               AND NOT EXISTS (SELECT 1 FROM strategy_execution_intents i WHERE i.signal_id=s.id)
            """),
            {"signals": removed_signals},
        )).rowcount or 0

    deleted_orphan_snapshots = 0
    if candidate_snapshot_ids:
        deleted_orphan_snapshots = (await db.execute(
            text("""
            DELETE FROM strategy_market_snapshots s
             WHERE s.id = ANY(CAST(:snapshots AS uuid[]))
               AND NOT EXISTS (SELECT 1 FROM strategy_orders o WHERE o.snapshot_id=s.id)
               AND NOT EXISTS (SELECT 1 FROM trades t WHERE t.strategy_snapshot_id=s.id)
               AND NOT EXISTS (SELECT 1 FROM strategy_reversal_intents r WHERE r.snapshot_id=s.id)
               AND NOT EXISTS (SELECT 1 FROM strategy_signals sig WHERE sig.snapshot_id=s.id)
               AND NOT EXISTS (SELECT 1 FROM strategy_execution_intents i WHERE i.snapshot_id=s.id)
            """),
            {"snapshots": candidate_snapshot_ids},
        )).rowcount or 0

    return {
        "deleted_live_trades": deleted_live_trades,
        "deleted_live_orders": deleted_live_orders,
        "deleted_live_intents": deleted_live_intents,
        "deleted_live_events": deleted_live_events,
        "deleted_live_risk_decisions": deleted_live_risk_decisions,
        "deleted_orphan_signals": deleted_orphan_signals,
        "deleted_orphan_snapshots": deleted_orphan_snapshots,
    }


@router.delete("/users/trade-logs/")
async def clear_trade_logs(
    request: Request,
    payload: dict,
    _: Principal = Depends(admin_only),
    db: AsyncSession = Depends(get_db),
):
    username = str(payload.get("username", "")).strip()
    scope = str(payload.get("scope", "demo")).lower()
    if scope not in {"demo", "live", "all"}:
        raise DomainError(400, "Scope must be demo, live, or all.")
    target = (
        await db.execute(
            text("SELECT id FROM users WHERE LOWER(username)=LOWER(:username)"),
            {"username": username},
        )
    ).scalar()
    if not target:
        raise DomainError(404, "User was not found.")
    target_id = str(target)

    # Global and user advisory locks matching Rust safety contract
    await db.execute(text("SELECT pg_advisory_xact_lock_shared(hashtext('rulenix:risk:global'))"))
    await db.execute(text("SELECT pg_advisory_xact_lock(hashtextextended(:target, 0))"), {"target": target_id})

    kill = bool(
        await db.scalar(
            text("SELECT COALESCE(enabled,FALSE) FROM risk_kill_switches WHERE user_id IS NULL")
        )
    )
    if not kill:
        raise DomainError(400, "Clear Trades requires the global kill switch to be enabled.")

    if scope in {"live", "all"}:
        await _verify_no_live_clear_blockers(db, target_id)
        await _verify_broker_safe_for_live_clear(request, db, target_id)
        await _verify_no_live_clear_blockers(db, target_id)

    cleared_demo = {}
    if scope in {"demo", "all"}:
        cleared_demo = await _clear_user_demo_trade_state(db, target_id)

    cleared_live = {}
    if scope in {"live", "all"}:
        cleared_live = await _clear_user_live_trade_state(db, target_id)

    deleted_backtest_runs = 0
    deleted_backtest_trades = 0
    if scope in {"demo", "all"}:
        deleted_backtest_trades = int(
            await db.scalar(
                text("SELECT COUNT(trade.id) FROM backtest_runs run JOIN backtest_trades trade ON trade.run_id=run.id WHERE run.user_id=:user"),
                {"user": target_id},
            ) or 0
        )
        deleted_runs_result = await db.execute(
            text("DELETE FROM backtest_runs WHERE user_id=:user"),
            {"user": target_id},
        )
        deleted_backtest_runs = int(deleted_runs_result.rowcount or 0)

    deleted_demo_trades = cleared_demo.get("deleted_demo_trades", 0)
    deleted_live_trades = cleared_live.get("deleted_live_trades", 0)
    deleted_trades = deleted_demo_trades + deleted_live_trades

    await db.commit()

    return {
        "detail": "Eligible local trading records cleared successfully. No broker order or position was changed.",
        "username": username,
        "scope": scope,
        "deleted_trades": deleted_trades,
        "deleted_demo_trades": deleted_demo_trades,
        "deleted_live_trades": deleted_live_trades,
        "deleted_closed_live_trades": deleted_live_trades,
        "deleted_demo_orders": cleared_demo.get("deleted_demo_orders", 0),
        "deleted_live_orders": cleared_live.get("deleted_live_orders", 0),
        "deleted_demo_intents": cleared_demo.get("deleted_demo_intents", 0),
        "deleted_live_intents": cleared_live.get("deleted_live_intents", 0),
        "skipped_ambiguous_intents": cleared_demo.get("skipped_ambiguous_intents", 0),
        "deleted_demo_events": cleared_demo.get("deleted_demo_events", 0),
        "deleted_live_events": cleared_live.get("deleted_live_events", 0),
        "deleted_demo_risk_decisions": cleared_demo.get("deleted_demo_risk_decisions", 0),
        "deleted_live_risk_decisions": cleared_live.get("deleted_live_risk_decisions", 0),
        "deleted_orphan_signals": cleared_demo.get("deleted_orphan_signals", 0) + cleared_live.get("deleted_orphan_signals", 0),
        "deleted_orphan_snapshots": cleared_demo.get("deleted_orphan_snapshots", 0) + cleared_live.get("deleted_orphan_snapshots", 0),
        "deleted_backtest_runs": deleted_backtest_runs,
        "deleted_backtest_trades": deleted_backtest_trades,
        "broker_mutations": {"placed": 0, "modified": 0, "cancelled": 0},
        "closed_live_history_cleanup": scope in {"live", "all"},
        "global_kill_switch": True,
    }


@router.get("/trades/daily/")
async def daily_trades(date: Date | None = Query(None), _: Principal = Depends(admin_only), db: AsyncSession = Depends(get_db)):
    if date is None:
        date = await db.scalar(text("SELECT (NOW() AT TIME ZONE 'Asia/Kolkata')::date"))
    rows = (await db.execute(text("""
      WITH pnl_counts AS (
        SELECT user_id,COUNT(*)::bigint AS pnl_trades,
               COUNT(*) FILTER (WHERE execution_mode='demo')::bigint AS demo_trades,
               COUNT(*) FILTER (WHERE execution_mode='live')::bigint AS live_trades,
               COUNT(*) FILTER (WHERE status='open')::bigint AS open_trades,
               COUNT(*) FILTER (WHERE status='closed')::bigint AS closed_trades
          FROM trades
         WHERE (entry_datetime AT TIME ZONE 'Asia/Kolkata')::date=CAST(:date AS date)
         GROUP BY user_id
      ), backtest_counts AS (
        SELECT run.user_id,COUNT(trade.id)::bigint AS backtest_trades
          FROM backtest_runs run JOIN backtest_trades trade ON trade.run_id=run.id
         WHERE (trade.entry_time AT TIME ZONE 'Asia/Kolkata')::date=CAST(:date AS date)
         GROUP BY run.user_id
      )
      SELECT u.id AS user_id,u.username,
             (COALESCE(p.pnl_trades,0)+COALESCE(b.backtest_trades,0))::bigint AS total_trades,
             COALESCE(p.pnl_trades,0)::bigint AS pnl_trades,
             COALESCE(b.backtest_trades,0)::bigint AS backtest_trades,
             COALESCE(p.demo_trades,0)::bigint AS demo_trades,
             COALESCE(p.live_trades,0)::bigint AS live_trades,
             COALESCE(p.open_trades,0)::bigint AS open_trades,
             COALESCE(p.closed_trades,0)::bigint AS closed_trades
        FROM users u LEFT JOIN pnl_counts p ON p.user_id=u.id
        LEFT JOIN backtest_counts b ON b.user_id=u.id
       ORDER BY (COALESCE(p.pnl_trades,0)+COALESCE(b.backtest_trades,0)) DESC,u.username
    """), {"date": date})).mappings().all()
    users = [dict(row) for row in rows]
    return {
        "date": date.isoformat(),
        "timezone": "Asia/Kolkata",
        "total_trades": sum(row["total_trades"] for row in users),
        "pnl_trades": sum(row["pnl_trades"] for row in users),
        "backtest_trades": sum(row["backtest_trades"] for row in users),
        "demo_trades": sum(row["demo_trades"] for row in users),
        "live_trades": sum(row["live_trades"] for row in users),
        "users": users,
    }
