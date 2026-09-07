from contextlib import asynccontextmanager
from pathlib import Path
from uuid import uuid4

from fastapi import FastAPI, Request
from fastapi.exceptions import RequestValidationError
from fastapi.middleware.cors import CORSMiddleware

from .api import account, admin, auth, deferred, egress, health, risk
from .broker.angel.egress import rehydrate_configured_ips
from .broker.angel.helper import EgressHelperClient
from .config import get_settings
from .db import make_engine, make_session_factory
from .errors import DomainError, domain_error_handler, validation_error_handler


@asynccontextmanager
async def lifespan(app: FastAPI):
    app.state.settings = get_settings()
    app.state.engine = make_engine(app.state.settings.async_database_url)
    app.state.session_factory = make_session_factory(app.state.engine)
    app.state.last_dev_otp = None
    if app.state.session_factory and Path(app.state.settings.egress_helper_socket).exists():
        async with app.state.session_factory() as session:
            await rehydrate_configured_ips(session, EgressHelperClient(app.state.settings.egress_helper_socket))
    yield
    if app.state.engine:
        await app.state.engine.dispose()

app = FastAPI(title="Rulenix Python Foundation API", version="0.1.0", lifespan=lifespan)
app.add_exception_handler(DomainError, domain_error_handler)
app.add_exception_handler(RequestValidationError, validation_error_handler)
app.add_middleware(CORSMiddleware, allow_origins=[get_settings().frontend_origin], allow_credentials=True, allow_methods=["*"], allow_headers=["*"])

@app.middleware("http")
async def request_context(request: Request, call_next):
    request_id = request.headers.get("X-Request-ID") or str(uuid4())
    response = await call_next(request)
    response.headers["X-Request-ID"] = request_id
    return response

for _router in (health.router, auth.router, account.router, admin.router, egress.router, risk.router, deferred.router):
    app.include_router(_router, prefix="/api")
