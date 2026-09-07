from uuid import UUID, uuid4

from fastapi import APIRouter, Depends, Path, Request
from sqlalchemy import text
from sqlalchemy.exc import IntegrityError
from sqlalchemy.ext.asyncio import AsyncSession

from ..broker.angel.egress import BROKEN_EGRESS_MESSAGE, validate_public_ipv4
from ..broker.angel.helper import EgressHelperClient
from ..dependencies import Principal, admin_only, get_db
from ..errors import DomainError

router = APIRouter(prefix="/admin", tags=["egress"])

INVENTORY = """
SELECT e.id,host(e.ip_address) AS ip_address,e.configuration_status,e.verification_status,
       e.status_message,e.configured_at,e.last_verified_at,e.created_at,e.updated_at,
       p.user_id AS assigned_user_id,u.username AS assigned_username,
       p.brokerage_user_id AS assigned_broker_account
  FROM broker_egress_ips e
  LEFT JOIN user_profiles p ON p.broker_egress_ip_id=e.id
  LEFT JOIN users u ON u.id=p.user_id
 ORDER BY e.ip_address
"""

async def _configure(db: AsyncSession, item_id: UUID, force: bool, socket_path: str = "/run/rulenix-egress/helper.sock") -> dict:
    row = (await db.execute(text("SELECT host(ip_address) AS ip_address,configuration_status,verification_status FROM broker_egress_ips WHERE id=:id FOR UPDATE"), {"id": item_id})).mappings().first()
    if not row:
        raise DomainError(404, "Egress IP not found.")
    if not force and row["configuration_status"] == "CONFIGURED" and row["verification_status"] == "VERIFIED":
        return dict((await db.execute(text(f"SELECT * FROM ({INVENTORY.rstrip(';')}) inventory WHERE id=:id"), {"id": item_id})).mappings().one())
    try:
        ip = validate_public_ipv4(row["ip_address"])
    except ValueError as exc:
        raise DomainError(400, str(exc)) from exc
    await db.execute(text("UPDATE broker_egress_ips SET configuration_status='CONFIGURING',verification_status='VERIFYING',status_message='',updated_at=NOW() WHERE id=:id"), {"id": item_id})
    # The helper is the only component allowed to alter host networking.
    try:
        result = await EgressHelperClient(socket_path).configure_and_verify(ip)
    except Exception as exc:
        await db.execute(text("UPDATE broker_egress_ips SET configuration_status='CONFIGURATION_FAILED',verification_status='UNVERIFIED',status_message=:message,updated_at=NOW() WHERE id=:id"), {"id": item_id, "message": str(exc)[:512]})
        raise DomainError(400, f"Could not configure Angel egress IP {ip}: {exc}") from exc
    if not result.configured:
        await db.execute(text("UPDATE broker_egress_ips SET configuration_status='CONFIGURATION_FAILED',verification_status='UNVERIFIED',status_message=:message,updated_at=NOW() WHERE id=:id"), {"id": item_id, "message": result.message})
        raise DomainError(400, f"Could not configure Angel egress IP {ip}: {result.message}")
    if not result.ok or not result.verified or result.observed_ip != str(ip):
        await db.execute(text("UPDATE broker_egress_ips SET verification_status='VERIFICATION_FAILED',status_message=:message,updated_at=NOW() WHERE id=:id"), {"id": item_id, "message": result.message})
        raise DomainError(400, f"Outbound verification failed for Angel egress IP {ip}: {result.message}")
    await db.execute(text("UPDATE broker_egress_ips SET configuration_status='CONFIGURED',verification_status='VERIFIED',status_message='',configured_at=COALESCE(configured_at,NOW()),last_verified_at=NOW(),updated_at=NOW() WHERE id=:id"), {"id": item_id})
    return dict((await db.execute(text(f"SELECT * FROM ({INVENTORY.rstrip(';')}) inventory WHERE id=:id"), {"id": item_id})).mappings().one())

@router.get("/egress-ips")
@router.get("/egress-ips/")
async def list_egress(_: Principal = Depends(admin_only), db: AsyncSession = Depends(get_db)):
    return [dict(row) for row in (await db.execute(text(INVENTORY))).mappings().all()]

@router.post("/egress-ips")
@router.post("/egress-ips/")
async def add_egress(request: Request, payload: dict, actor: Principal = Depends(admin_only), db: AsyncSession = Depends(get_db)):
    try:
        ip = validate_public_ipv4(str(payload.get("ip_address", "")))
    except ValueError as exc:
        raise DomainError(400, str(exc)) from exc
    item_id = uuid4()
    try:
        await db.execute(text("INSERT INTO broker_egress_ips(id,ip_address,created_by) VALUES(:id,CAST(:ip AS inet),:actor)"), {"id": item_id, "ip": str(ip), "actor": actor.id})
        await _configure(db, item_id, False, request.app.state.settings.egress_helper_socket)
        await db.commit()
    except IntegrityError as exc:
        await db.rollback()
        raise DomainError(400, "That egress IPv4 is already registered.") from exc
    return {"egress_ip": dict((await db.execute(text(f"SELECT * FROM ({INVENTORY.rstrip(';')}) inventory WHERE id=:id"), {"id": item_id})).mappings().one())}

@router.post("/egress-ips/{item_id}/verify")
async def verify_egress(request: Request, item_id: UUID = Path(...), _: Principal = Depends(admin_only), db: AsyncSession = Depends(get_db)):
    value = await _configure(db, item_id, True, request.app.state.settings.egress_helper_socket)
    await db.commit()
    return {"egress_ip": value}

@router.put("/users/{user_id}/angel-egress")
async def assign_egress(user_id: UUID, payload: dict, _: Principal = Depends(admin_only), db: AsyncSession = Depends(get_db)):
    item_id = payload.get("egress_ip_id")
    if item_id:
        try:
            item_id = UUID(str(item_id))
        except ValueError as exc:
            raise DomainError(422, "egress_ip_id must be a UUID.") from exc
    async with db.begin():
        await db.execute(text("SELECT pg_advisory_xact_lock(hashtextextended(CAST(:user_id AS text),0))"), {"user_id": str(user_id)})
        if item_id:
            await db.execute(text("SELECT pg_advisory_xact_lock(hashtextextended(CAST(:item_id AS text),1))"), {"item_id": str(item_id)})
            ready = await db.scalar(text("SELECT EXISTS(SELECT 1 FROM broker_egress_ips WHERE id=:id AND configuration_status='CONFIGURED' AND verification_status='VERIFIED')"), {"id": item_id})
            if not ready:
                raise DomainError(400, BROKEN_EGRESS_MESSAGE)
        result = await db.execute(text("UPDATE user_profiles SET broker_egress_ip_id=:item,updated_at=NOW() WHERE user_id=:user"), {"item": item_id, "user": user_id})
        if getattr(result, "rowcount", 0) != 1:
            raise DomainError(404, "Angel broker account not found.")
    return {"user_id": str(user_id), "egress_mode": "explicit" if item_id else "default", "egress_ip_id": str(item_id) if item_id else None}
