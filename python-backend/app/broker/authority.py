"""Database-backed, cross-runtime LIVE mutation authority fencing."""

from __future__ import annotations

from collections.abc import AsyncIterator
from contextlib import asynccontextmanager
from dataclasses import dataclass
from datetime import datetime
from enum import StrEnum
from uuid import UUID

from sqlalchemy import text
from sqlalchemy.ext.asyncio import AsyncSession, async_sessionmaker


class AuthorityRuntime(StrEnum):
    RUST = "rust"
    PYTHON = "python"
    NONE = "none"


class AuthorityError(RuntimeError):
    """Raised before transport when the caller does not own valid authority."""


@dataclass(frozen=True)
class AuthorityProof:
    runtime: AuthorityRuntime
    epoch: int
    lease_owner: UUID
    lease_expires_at: datetime
    user_id: UUID


@dataclass(frozen=True)
class AuthorityState:
    holder: AuthorityRuntime
    epoch: int
    lease_owner: UUID | None
    lease_expires_at: datetime
    updated_at: datetime
    updated_by: str


_LOCK_SQL = "hashtext('rulenix:live-mutation-authority')"


def _state(row) -> AuthorityState:
    return AuthorityState(
        holder=AuthorityRuntime(str(row["holder"])),
        epoch=int(row["epoch"]),
        lease_owner=row["lease_owner"],
        lease_expires_at=row["lease_expires_at"],
        updated_at=row["updated_at"],
        updated_by=str(row["updated_by"]),
    )


class LiveMutationAuthority:
    """Holds a shared transaction lock for the entire broker write.

    Authority transfer takes the matching exclusive transaction lock.  A
    paused worker therefore either completes before transfer or resumes after
    transfer and observes that its authority is stale.
    """

    def __init__(self, session_factory: async_sessionmaker[AsyncSession]):
        self.session_factory = session_factory

    async def current(self) -> AuthorityState:
        async with self.session_factory() as session:
            row = (
                await session.execute(
                    text("""
                    SELECT holder,epoch,lease_owner,lease_expires_at,updated_at,updated_by
                      FROM live_mutation_authority WHERE singleton=TRUE
                    """)
                )
            ).mappings().one()
            return _state(row)

    @asynccontextmanager
    async def mutation_permit(
        self, *, runtime: AuthorityRuntime, lease_owner: UUID, user_id: UUID
    ) -> AsyncIterator[AuthorityProof]:
        if runtime is AuthorityRuntime.NONE:
            raise AuthorityError("The NONE runtime can never mutate a broker account.")
        async with self.session_factory() as session, session.begin():
            await session.execute(text(f"SELECT pg_advisory_xact_lock_shared({_LOCK_SQL})"))
            row = (
                await session.execute(
                    text("""
                    SELECT holder,epoch,lease_owner,lease_expires_at,updated_at,updated_by,
                           lease_expires_at > clock_timestamp() AS lease_valid
                      FROM live_mutation_authority WHERE singleton=TRUE FOR SHARE
                    """)
                )
            ).mappings().one()
            if (
                str(row["holder"]) != runtime.value
                or row["lease_owner"] != lease_owner
                or not bool(row["lease_valid"])
            ):
                raise AuthorityError(
                    "LIVE mutation authority is absent, stale, expired, or owned by another runtime."
                )
            yield AuthorityProof(
                runtime=runtime,
                epoch=int(row["epoch"]),
                lease_owner=lease_owner,
                lease_expires_at=row["lease_expires_at"],
                user_id=user_id,
            )

    async def transfer(
        self,
        *,
        expected_holder: AuthorityRuntime,
        expected_epoch: int,
        expected_lease_owner: UUID,
        new_holder: AuthorityRuntime,
        new_lease_owner: UUID | None,
        lease_seconds: int | None,
        reason: str,
    ) -> AuthorityState:
        if not reason.strip():
            raise ValueError("Authority transfer requires an audit reason.")
        if new_holder is AuthorityRuntime.NONE:
            if new_lease_owner is not None:
                raise ValueError("Fenced authority cannot have a lease owner.")
        elif new_lease_owner is None:
            raise ValueError("An active authority requires a lease owner.")
        if lease_seconds is not None and not 5 <= lease_seconds <= 300:
            raise ValueError("Authority lease must be between 5 and 300 seconds.")

        async with self.session_factory() as session, session.begin():
            await session.execute(text(f"SELECT pg_advisory_xact_lock({_LOCK_SQL})"))
            before_row = (
                await session.execute(
                    text("""
                    SELECT holder,epoch,lease_owner,lease_expires_at,updated_at,updated_by
                      FROM live_mutation_authority WHERE singleton=TRUE FOR UPDATE
                    """)
                )
            ).mappings().one()
            before = _state(before_row)
            if (
                before.holder is not expected_holder
                or before.epoch != expected_epoch
                or before.lease_owner != expected_lease_owner
            ):
                raise AuthorityError("Authority changed before the requested transfer.")
            if new_holder is AuthorityRuntime.RUST and lease_seconds is None:
                expiry_sql = "'infinity'::timestamptz"
            elif new_holder is AuthorityRuntime.NONE:
                expiry_sql = "clock_timestamp()"
            else:
                if lease_seconds is None:
                    raise ValueError("Python authority requires a bounded lease.")
                expiry_sql = "clock_timestamp() + (:lease_seconds * INTERVAL '1 second')"
            new_epoch = before.epoch + 1
            row = (
                await session.execute(
                    text(f"""
                    UPDATE live_mutation_authority
                       SET holder=:holder,epoch=:epoch,lease_owner=:owner,
                           lease_expires_at={expiry_sql},updated_at=clock_timestamp(),
                           updated_by=:reason
                     WHERE singleton=TRUE
                    RETURNING holder,epoch,lease_owner,lease_expires_at,updated_at,updated_by
                    """),
                    {
                        "holder": new_holder.value,
                        "epoch": new_epoch,
                        "owner": new_lease_owner,
                        "lease_seconds": lease_seconds,
                        "reason": reason[:1000],
                    },
                )
            ).mappings().one()
            await session.execute(
                text("""
                INSERT INTO live_mutation_authority_events(
                    previous_holder,new_holder,previous_epoch,new_epoch,
                    previous_lease_owner,new_lease_owner,reason)
                VALUES(:previous_holder,:new_holder,:previous_epoch,:new_epoch,
                       :previous_owner,:new_owner,:reason)
                """),
                {
                    "previous_holder": before.holder.value,
                    "new_holder": new_holder.value,
                    "previous_epoch": before.epoch,
                    "new_epoch": new_epoch,
                    "previous_owner": before.lease_owner,
                    "new_owner": new_lease_owner,
                    "reason": reason[:1000],
                },
            )
            return _state(row)

    async def renew_python(
        self, *, epoch: int, lease_owner: UUID, lease_seconds: int = 30
    ) -> AuthorityState:
        if not 5 <= lease_seconds <= 300:
            raise ValueError("Authority lease must be between 5 and 300 seconds.")
        async with self.session_factory() as session, session.begin():
            await session.execute(text(f"SELECT pg_advisory_xact_lock({_LOCK_SQL})"))
            row = (
                await session.execute(
                    text("""
                    UPDATE live_mutation_authority
                       SET lease_expires_at=clock_timestamp()+(:seconds*INTERVAL '1 second'),
                           updated_at=clock_timestamp(),updated_by='python lease renewal'
                     WHERE singleton=TRUE AND holder='python' AND epoch=:epoch
                       AND lease_owner=:owner AND lease_expires_at>clock_timestamp()
                    RETURNING holder,epoch,lease_owner,lease_expires_at,updated_at,updated_by
                    """),
                    {"seconds": lease_seconds, "epoch": epoch, "owner": lease_owner},
                )
            ).mappings().first()
            if row is None:
                raise AuthorityError("Python authority lease is stale or expired.")
            return _state(row)


__all__ = [
    "AuthorityError",
    "AuthorityProof",
    "AuthorityRuntime",
    "AuthorityState",
    "LiveMutationAuthority",
]
