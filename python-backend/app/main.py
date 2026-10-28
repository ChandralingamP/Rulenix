from contextlib import asynccontextmanager
from pathlib import Path
from uuid import uuid4

from fastapi import FastAPI, Request
from fastapi.exceptions import RequestValidationError
from fastapi.middleware.cors import CORSMiddleware
from starlette.exceptions import HTTPException as StarletteHTTPException

from .api import (
    account,
    admin,
    auth,
    backtesting,
    egress,
    health,
    home,
    jobs,
    logs,
    pnl,
    risk,
    strategy,
    ws,
)
from .broker.angel.egress import rehydrate_configured_ips
from .broker.angel.helper import EgressHelperClient
from .config import get_settings
from .db import make_engine, make_session_factory
from .errors import DomainError, domain_error_handler, http_error_handler, validation_error_handler
from .reconciliation.workers import BackgroundWorkerManager
from .runtime.service import ProductionRuntime
from .strategy.runtime import SchedulerHealth


async def bootstrap_initial_admin(session_factory, settings):
    from uuid import uuid4
    from sqlalchemy import text
    from .security import hash_password

    username = settings.initial_admin_username.strip()
    email = settings.initial_admin_email.strip().lower()
    password = settings.initial_admin_password.strip()
    if not username or not email or not password:
        return
    async with session_factory() as session:
        existing = await session.execute(
            text("SELECT id FROM users WHERE LOWER(username)=LOWER(:username)"),
            {"username": username},
        )
        row = existing.mappings().first()
        pwd_hash = hash_password(password)
        if row:
            await session.execute(
                text("""
                UPDATE users SET email=:email, password_hash=:pwd, can_administer=TRUE,
                       is_active=TRUE, can_live_trade=TRUE, can_backtest=TRUE,
                       can_backtest_on_trading_days=TRUE, updated_at=NOW()
                 WHERE id=:id
                """),
                {"id": row["id"], "email": email, "pwd": pwd_hash},
            )
        else:
            user_id = uuid4()
            await session.execute(
                text("""
                INSERT INTO users (id, username, email, password_hash, is_active, is_staff, is_superuser, can_administer, can_live_trade, can_backtest, can_backtest_on_trading_days)
                VALUES (:id, :username, :email, :pwd, TRUE, TRUE, TRUE, TRUE, TRUE, TRUE, TRUE)
                """),
                {
                    "id": user_id,
                    "username": username.upper(),
                    "email": email,
                    "pwd": pwd_hash,
                },
            )
            await session.execute(
                text("""
                INSERT INTO user_profiles (user_id, trading_mode, demo_balance)
                VALUES (:id, 'demo', 100000.00)
                ON CONFLICT (user_id) DO NOTHING
                """),
                {"id": user_id},
            )
        await session.commit()


@asynccontextmanager
async def lifespan(app: FastAPI):
    app.state.settings = get_settings()
    app.state.engine = make_engine(app.state.settings.async_database_url)
    app.state.session_factory = make_session_factory(app.state.engine)
    if (
        app.state.session_factory
        and app.state.settings.initial_admin_username
        and app.state.settings.initial_admin_email
        and app.state.settings.initial_admin_password
    ):
        await bootstrap_initial_admin(app.state.session_factory, app.state.settings)
    app.state.last_dev_otp = None
    app.state.worker_manager = BackgroundWorkerManager()
    app.state.scheduler_health = SchedulerHealth()
    app.state.production_runtime = None
    try:
        if app.state.session_factory and Path(app.state.settings.egress_helper_socket).exists():
            async with app.state.session_factory() as session:
                await rehydrate_configured_ips(
                    session,
                    EgressHelperClient(app.state.settings.egress_helper_socket),
                )
        if (
            app.state.engine is not None
            and app.state.session_factory is not None
            and app.state.settings.runtime_mode.strip().lower() != "off"
        ):
            app.state.production_runtime = ProductionRuntime(
                app.state.engine,
                app.state.session_factory,
                app.state.settings,
                app.state.scheduler_health,
            )
            await app.state.production_runtime.start()
        yield
    finally:
        if app.state.production_runtime is not None:
            await app.state.production_runtime.stop()
        await app.state.worker_manager.stop()
        if app.state.engine:
            await app.state.engine.dispose()

app = FastAPI(title="Rulenix Python Foundation API", version="0.1.0", lifespan=lifespan)
app.add_exception_handler(DomainError, domain_error_handler)
app.add_exception_handler(StarletteHTTPException, http_error_handler)
app.add_exception_handler(RequestValidationError, validation_error_handler)
app.add_middleware(
    CORSMiddleware,
    allow_origins=[o.strip() for o in get_settings().frontend_origin.split(",") if o.strip()],
    allow_credentials=True,
    allow_methods=["*"],
    allow_headers=["*"],
)

@app.middleware("http")
async def request_context(request: Request, call_next):
    request_id = request.headers.get("X-Request-ID") or str(uuid4())
    response = await call_next(request)
    response.headers["X-Request-ID"] = request_id
    return response

for _router in (health.router, auth.router, account.router, admin.router, egress.router, risk.router, home.router, pnl.router, strategy.router, backtesting.router, jobs.router, logs.router, ws.router):
    app.include_router(_router, prefix="/api")
