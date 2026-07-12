use crate::error::{AppError, AppResult};
use aes_gcm::{
    Aes256Gcm, Nonce,
    aead::{Aead, KeyInit, OsRng, Payload, rand_core::RngCore},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use sqlx::PgPool;
use std::{collections::HashMap, env, fmt, sync::Arc};
use uuid::Uuid;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

const SECRET_KINDS: [&str; 4] = ["api_key", "jwt_token", "refresh_token", "feed_token"];
type EncryptedSecretRow = (Uuid, String, i32, Vec<u8>, Vec<u8>);

#[derive(Default, Zeroize, ZeroizeOnDrop)]
pub struct BrokerCredentials {
    pub api_key: String,
    pub jwt_token: String,
    pub refresh_token: String,
    pub feed_token: String,
}

impl fmt::Debug for BrokerCredentials {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BrokerCredentials")
            .field("api_key", &"[REDACTED]")
            .field("jwt_token", &"[REDACTED]")
            .field("refresh_token", &"[REDACTED]")
            .field("feed_token", &"[REDACTED]")
            .finish()
    }
}

#[derive(Clone)]
pub struct CredentialStore {
    db: PgPool,
    cipher: Arc<CredentialCipher>,
}

struct CredentialCipher {
    primary_version: i32,
    keys: HashMap<i32, Aes256Gcm>,
}

impl CredentialCipher {
    fn from_env() -> anyhow::Result<Self> {
        let raw = Zeroizing::new(
            env::var("CREDENTIAL_ENCRYPTION_KEYS")
                .map_err(|_| anyhow::anyhow!("CREDENTIAL_ENCRYPTION_KEYS is required"))?,
        );
        let primary_version: i32 = env::var("CREDENTIAL_ENCRYPTION_PRIMARY_VERSION")
            .map_err(|_| anyhow::anyhow!("CREDENTIAL_ENCRYPTION_PRIMARY_VERSION is required"))?
            .parse()
            .map_err(|_| {
                anyhow::anyhow!("CREDENTIAL_ENCRYPTION_PRIMARY_VERSION must be an integer")
            })?;
        Self::from_encoded(primary_version, &raw)
    }

    fn from_encoded(primary_version: i32, encoded: &str) -> anyhow::Result<Self> {
        let mut keys = HashMap::new();
        for entry in encoded
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            let (version, material) = entry
                .split_once(':')
                .ok_or_else(|| anyhow::anyhow!("invalid credential key entry"))?;
            let version: i32 = version
                .parse()
                .map_err(|_| anyhow::anyhow!("invalid credential key version"))?;
            if version <= 0 || keys.contains_key(&version) {
                anyhow::bail!("credential key versions must be unique positive integers");
            }
            let decoded = Zeroizing::new(
                STANDARD
                    .decode(material)
                    .map_err(|_| anyhow::anyhow!("credential keys must be base64"))?,
            );
            if decoded.len() != 32 {
                anyhow::bail!("each credential encryption key must decode to exactly 32 bytes");
            }
            keys.insert(
                version,
                Aes256Gcm::new_from_slice(&decoded).expect("validated AES-256 key"),
            );
        }
        if !keys.contains_key(&primary_version) {
            anyhow::bail!(
                "primary credential key version is not present in CREDENTIAL_ENCRYPTION_KEYS"
            );
        }
        Ok(Self {
            primary_version,
            keys,
        })
    }

    fn aad(user_id: Uuid, kind: &str, version: i32) -> String {
        format!("rulenix:broker-secret:{user_id}:{kind}:v{version}")
    }

    fn encrypt(
        &self,
        user_id: Uuid,
        kind: &str,
        plaintext: &str,
    ) -> AppResult<(i32, Vec<u8>, Vec<u8>)> {
        let mut nonce = [0_u8; 12];
        OsRng.fill_bytes(&mut nonce);
        let version = self.primary_version;
        let ciphertext = self.keys[&version]
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: plaintext.as_bytes(),
                    aad: Self::aad(user_id, kind, version).as_bytes(),
                },
            )
            .map_err(|_| AppError::Internal(anyhow::anyhow!("credential encryption failed")))?;
        Ok((version, nonce.to_vec(), ciphertext))
    }

    fn decrypt(
        &self,
        user_id: Uuid,
        kind: &str,
        version: i32,
        nonce: &[u8],
        ciphertext: &[u8],
    ) -> AppResult<String> {
        let cipher = self.keys.get(&version).ok_or_else(|| {
            AppError::Internal(anyhow::anyhow!(
                "credential key version {version} is unavailable"
            ))
        })?;
        if nonce.len() != 12 {
            return Err(AppError::Internal(anyhow::anyhow!(
                "encrypted credential is corrupt"
            )));
        }
        let plaintext = cipher
            .decrypt(
                Nonce::from_slice(nonce),
                Payload {
                    msg: ciphertext,
                    aad: Self::aad(user_id, kind, version).as_bytes(),
                },
            )
            .map_err(|_| {
                AppError::Internal(anyhow::anyhow!(
                    "encrypted credential authentication failed"
                ))
            })?;
        String::from_utf8(plaintext)
            .map_err(|_| AppError::Internal(anyhow::anyhow!("encrypted credential is corrupt")))
    }
}

impl CredentialStore {
    pub fn from_env(db: PgPool) -> anyhow::Result<Self> {
        Ok(Self {
            db,
            cipher: Arc::new(CredentialCipher::from_env()?),
        })
    }

    pub async fn load(&self, user_id: Uuid) -> AppResult<BrokerCredentials> {
        let rows: Vec<(String, i32, Vec<u8>, Vec<u8>)> = sqlx::query_as(
            "SELECT secret_kind,key_version,nonce,ciphertext FROM broker_secrets WHERE user_id=$1",
        )
        .bind(user_id)
        .fetch_all(&self.db)
        .await?;
        let mut result = BrokerCredentials::default();
        for (kind, version, nonce, ciphertext) in rows {
            let value = self
                .cipher
                .decrypt(user_id, &kind, version, &nonce, &ciphertext)?;
            match kind.as_str() {
                "api_key" => result.api_key = value,
                "jwt_token" => result.jwt_token = value,
                "refresh_token" => result.refresh_token = value,
                "feed_token" => result.feed_token = value,
                _ => {
                    return Err(AppError::Internal(anyhow::anyhow!(
                        "unknown encrypted credential kind"
                    )));
                }
            }
        }
        Ok(result)
    }

    pub async fn put(&self, user_id: Uuid, values: &[(&str, &str)]) -> AppResult<()> {
        let mut transaction = self.db.begin().await?;
        for (kind, value) in values {
            if !SECRET_KINDS.contains(kind) {
                return Err(AppError::Internal(anyhow::anyhow!(
                    "invalid credential kind"
                )));
            }
            if value.is_empty() {
                sqlx::query("DELETE FROM broker_secrets WHERE user_id=$1 AND secret_kind=$2")
                    .bind(user_id)
                    .bind(kind)
                    .execute(&mut *transaction)
                    .await?;
            } else {
                let (version, nonce, ciphertext) = self.cipher.encrypt(user_id, kind, value)?;
                sqlx::query("INSERT INTO broker_secrets (user_id,secret_kind,key_version,nonce,ciphertext) VALUES ($1,$2,$3,$4,$5) ON CONFLICT (user_id,secret_kind) DO UPDATE SET key_version=EXCLUDED.key_version,nonce=EXCLUDED.nonce,ciphertext=EXCLUDED.ciphertext,updated_at=NOW()")
                    .bind(user_id).bind(kind).bind(version).bind(nonce).bind(ciphertext).execute(&mut *transaction).await?;
            }
        }
        transaction.commit().await?;
        Ok(())
    }

    pub async fn clear_tokens(&self, user_id: Uuid) -> AppResult<()> {
        self.put(
            user_id,
            &[("jwt_token", ""), ("refresh_token", ""), ("feed_token", "")],
        )
        .await
    }

    pub async fn migrate_plaintext(&self) -> AppResult<u64> {
        let mut transaction = self.db.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext('rulenix_broker_secret_migration'))")
            .execute(&mut *transaction)
            .await?;
        let rows: Vec<(Uuid, String, String, String, String)> = sqlx::query_as(
            "SELECT user_id,api_key,jwt_token,refresh_token,feed_token FROM user_profiles WHERE api_key<>'' OR jwt_token<>'' OR refresh_token<>'' OR feed_token<>'' FOR UPDATE",
        )
        .fetch_all(&mut *transaction)
        .await?;
        let mut migrated = 0_u64;
        for (user_id, api_key, jwt, refresh, feed) in rows {
            let api_key = Zeroizing::new(api_key);
            let jwt = Zeroizing::new(jwt);
            let refresh = Zeroizing::new(refresh);
            let feed = Zeroizing::new(feed);
            for (kind, value) in [
                ("api_key", api_key.as_str()),
                ("jwt_token", jwt.as_str()),
                ("refresh_token", refresh.as_str()),
                ("feed_token", feed.as_str()),
            ] {
                if !value.is_empty() {
                    let (version, nonce, ciphertext) = self.cipher.encrypt(user_id, kind, value)?;
                    sqlx::query("INSERT INTO broker_secrets (user_id,secret_kind,key_version,nonce,ciphertext) VALUES ($1,$2,$3,$4,$5) ON CONFLICT (user_id,secret_kind) DO UPDATE SET key_version=EXCLUDED.key_version,nonce=EXCLUDED.nonce,ciphertext=EXCLUDED.ciphertext,updated_at=NOW()")
                        .bind(user_id).bind(kind).bind(version).bind(nonce).bind(ciphertext).execute(&mut *transaction).await?;
                }
            }
            sqlx::query("UPDATE user_profiles SET api_key='',jwt_token='',refresh_token='',feed_token='',updated_at=NOW() WHERE user_id=$1")
                .bind(user_id).execute(&mut *transaction).await?;
            migrated += 1;
        }
        sqlx::query("DO $$ BEGIN IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname='user_profiles_no_plaintext_credentials') THEN ALTER TABLE user_profiles ADD CONSTRAINT user_profiles_no_plaintext_credentials CHECK (api_key='' AND jwt_token='' AND refresh_token='' AND feed_token=''); END IF; END $$")
            .execute(&mut *transaction).await?;
        transaction.commit().await?;
        Ok(migrated)
    }

    pub async fn rotate_all(&self) -> AppResult<u64> {
        let rows: Vec<EncryptedSecretRow> = sqlx::query_as(
            "SELECT user_id,secret_kind,key_version,nonce,ciphertext FROM broker_secrets WHERE key_version<>$1",
        ).bind(self.cipher.primary_version).fetch_all(&self.db).await?;
        let mut rotated = 0_u64;
        for (user_id, kind, version, nonce, ciphertext) in rows {
            let plaintext = Zeroizing::new(self.cipher.decrypt(
                user_id,
                &kind,
                version,
                &nonce,
                &ciphertext,
            )?);
            let (new_version, new_nonce, new_ciphertext) =
                self.cipher.encrypt(user_id, &kind, &plaintext)?;
            sqlx::query("UPDATE broker_secrets SET key_version=$1,nonce=$2,ciphertext=$3,updated_at=NOW() WHERE user_id=$4 AND secret_kind=$5 AND key_version=$6")
                .bind(new_version).bind(new_nonce).bind(new_ciphertext).bind(user_id).bind(&kind).bind(version).execute(&self.db).await?;
            rotated += 1;
        }
        Ok(rotated)
    }
}

pub fn redact_sensitive(message: &str, values: &[&str]) -> String {
    values
        .iter()
        .filter(|value| !value.is_empty())
        .fold(message.to_owned(), |result, value| {
            result.replace(value, "[REDACTED]")
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(byte: u8) -> String {
        STANDARD.encode([byte; 32])
    }
    fn cipher(primary: i32) -> CredentialCipher {
        CredentialCipher::from_encoded(primary, &format!("2:{},1:{}", key(2), key(1))).unwrap()
    }

    #[test]
    fn encryption_round_trip_and_random_nonce() {
        let user = Uuid::new_v4();
        let cipher = cipher(1);
        let first = cipher.encrypt(user, "api_key", "secret-value").unwrap();
        let second = cipher.encrypt(user, "api_key", "secret-value").unwrap();
        assert_ne!(first.1, second.1);
        assert_ne!(first.2, b"secret-value");
        assert_eq!(
            cipher
                .decrypt(user, "api_key", first.0, &first.1, &first.2)
                .unwrap(),
            "secret-value"
        );
    }

    #[test]
    fn corruption_and_wrong_context_are_rejected() {
        let user = Uuid::new_v4();
        let cipher = cipher(1);
        let (version, nonce, mut ciphertext) = cipher.encrypt(user, "jwt_token", "token").unwrap();
        ciphertext[0] ^= 1;
        assert!(
            cipher
                .decrypt(user, "jwt_token", version, &nonce, &ciphertext)
                .is_err()
        );
        let valid = cipher.encrypt(user, "jwt_token", "token").unwrap();
        assert!(
            cipher
                .decrypt(Uuid::new_v4(), "jwt_token", valid.0, &valid.1, &valid.2)
                .is_err()
        );
    }

    #[test]
    fn old_key_decrypts_while_new_key_encrypts() {
        let user = Uuid::new_v4();
        let old_only = CredentialCipher::from_encoded(1, &format!("1:{}", key(1))).unwrap();
        let new_only = CredentialCipher::from_encoded(2, &format!("2:{}", key(2))).unwrap();
        let old = old_only.encrypt(user, "feed_token", "feed").unwrap();
        let rotating = cipher(2);
        assert_eq!(
            rotating
                .decrypt(user, "feed_token", old.0, &old.1, &old.2)
                .unwrap(),
            "feed"
        );
        let new = rotating.encrypt(user, "feed_token", "feed").unwrap();
        assert_eq!(new.0, 2);
        assert_eq!(
            new_only
                .decrypt(user, "feed_token", new.0, &new.1, &new.2)
                .unwrap(),
            "feed"
        );
        assert!(
            old_only
                .decrypt(user, "feed_token", new.0, &new.1, &new.2)
                .is_err()
        );
    }

    #[test]
    fn debug_and_message_redaction_hide_secrets() {
        let credentials = BrokerCredentials {
            api_key: "API-SECRET".into(),
            jwt_token: "JWT-SECRET".into(),
            refresh_token: "REFRESH".into(),
            feed_token: "FEED".into(),
        };
        let debug = format!("{credentials:?}");
        assert!(!debug.contains("API-SECRET"));
        assert_eq!(
            redact_sensitive(
                "headers Bearer JWT-SECRET key API-SECRET mpin 1234 totp 654321",
                &["Bearer JWT-SECRET", "API-SECRET", "1234", "654321"]
            ),
            "headers [REDACTED] key [REDACTED] mpin [REDACTED] totp [REDACTED]"
        );
    }
}
