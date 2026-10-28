import asyncio
from uuid import uuid4
from sqlalchemy import text
from app.config import get_settings
from app.db import make_engine, make_session_factory
from app.security import hash_password, encrypt_broker_secret

async def main():
    settings = get_settings()
    engine = make_engine(settings.async_database_url)
    session_factory = make_session_factory(engine)

    username = "CHANDRU"
    email = "chandralingam2003@gmail.com"
    password = "#Rulenix@2026"
    api_key = "QrZLQV90"
    client_id = "IIRA142474"
    mobile = "9876543210"

    async with session_factory() as session:
        # Check if user already exists
        row = (
            await session.execute(
                text("SELECT id FROM users WHERE LOWER(username)=LOWER(:username) OR LOWER(email)=LOWER(:email)"),
                {"username": username, "email": email},
            )
        ).mappings().first()

        pwd_hash = hash_password(password)

        if row:
            user_id = row["id"]
            print(f"User already exists with ID: {user_id}. Updating details...")
            await session.execute(
                text("""
                UPDATE users SET username=:username, email=:email, password_hash=:pwd,
                       is_active=TRUE, can_live_trade=TRUE, can_backtest=TRUE,
                       can_backtest_on_trading_days=TRUE, updated_at=NOW()
                 WHERE id=:id
                """),
                {"id": user_id, "username": username, "email": email, "pwd": pwd_hash},
            )
        else:
            user_id = uuid4()
            print(f"Creating new user with ID: {user_id}...")
            await session.execute(
                text("""
                INSERT INTO users (id, username, email, password_hash, is_active, is_staff, is_superuser, can_administer, can_live_trade, can_backtest, can_backtest_on_trading_days)
                VALUES (:id, :username, :email, :pwd, TRUE, FALSE, FALSE, FALSE, TRUE, TRUE, TRUE)
                """),
                {"id": user_id, "username": username, "email": email, "pwd": pwd_hash},
            )

        # Upsert user_profiles
        await session.execute(
            text("""
            INSERT INTO user_profiles (user_id, brokerage_user_id, api_key, mobile_number, trading_mode, demo_balance)
            VALUES (:id, :client_id, '', :mobile, 'demo', 100000.00)
            ON CONFLICT (user_id) DO UPDATE SET
                brokerage_user_id = EXCLUDED.brokerage_user_id,
                mobile_number = EXCLUDED.mobile_number,
                updated_at = NOW()
            """),
            {"id": user_id, "client_id": client_id, "mobile": mobile},
        )

        # Encrypt and store API key in broker_secrets
        key_ver, nonce, ciphertext = encrypt_broker_secret(
            api_key,
            str(user_id),
            "api_key",
            settings.credential_keys,
            settings.credential_primary_version,
        )

        await session.execute(
            text("""
            INSERT INTO broker_secrets (user_id, secret_kind, key_version, nonce, ciphertext)
            VALUES (:id, 'api_key', :ver, :nonce, :ciphertext)
            ON CONFLICT (user_id, secret_kind) DO UPDATE SET
                key_version = EXCLUDED.key_version,
                nonce = EXCLUDED.nonce,
                ciphertext = EXCLUDED.ciphertext,
                updated_at = NOW()
            """),
            {"id": user_id, "ver": key_ver, "nonce": nonce, "ciphertext": ciphertext},
        )

        await session.commit()
        print("User successfully saved to database.")

    await engine.dispose()

if __name__ == "__main__":
    asyncio.run(main())

