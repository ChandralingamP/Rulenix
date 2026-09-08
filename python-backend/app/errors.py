from dataclasses import dataclass
from typing import Any

from fastapi import Request
from fastapi.exceptions import RequestValidationError
from fastapi.responses import JSONResponse
from starlette.exceptions import HTTPException as StarletteHTTPException


@dataclass
class DomainError(Exception):
    status_code: int
    detail: str
    retry_after: int | None = None
    code: str | None = None


def error_payload(error: DomainError) -> dict[str, Any]:
    payload: dict[str, Any] = {"detail": error.detail, "retry_after": error.retry_after}
    if error.code:
        payload["code"] = error.code
    return payload


async def domain_error_handler(_: Request, error: Exception) -> JSONResponse:
    assert isinstance(error, DomainError)
    return JSONResponse(error_payload(error), status_code=error.status_code)


async def validation_error_handler(_: Request, error: Exception) -> JSONResponse:
    assert isinstance(error, RequestValidationError)
    first = error.errors()[0] if error.errors() else {}
    msg = str(first.get("msg", "Invalid request"))
    return JSONResponse({"detail": msg, "retry_after": None}, status_code=422)


async def http_error_handler(_: Request, error: Exception) -> JSONResponse:
    assert isinstance(error, StarletteHTTPException)
    detail = error.detail if isinstance(error.detail, str) else "Request failed."
    return JSONResponse({"detail": detail, "retry_after": None}, status_code=error.status_code, headers=getattr(error, "headers", None))
