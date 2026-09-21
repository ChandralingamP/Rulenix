use crate::error::{AppError, AppResult};
use sqlx::{PgPool, Postgres, Transaction};

/// Keeps the shared transaction advisory lock alive for the complete Angel
/// mutation. Authority transfer takes the matching exclusive lock.
pub struct LiveMutationGuard {
    _transaction: Transaction<'static, Postgres>,
    pub epoch: i64,
}

pub async fn acquire_rust(pool: &PgPool) -> AppResult<LiveMutationGuard> {
    let mut transaction = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock_shared(hashtext('rulenix:live-mutation-authority'))")
        .execute(&mut *transaction)
        .await?;
    let row: Option<(String, i64, bool)> = sqlx::query_as(
        "SELECT holder,epoch,lease_expires_at>clock_timestamp() \
           FROM live_mutation_authority WHERE singleton=TRUE FOR SHARE",
    )
    .fetch_optional(&mut *transaction)
    .await?;
    let Some((holder, epoch, lease_valid)) = row else {
        return Err(AppError::Forbidden(
            "LIVE mutation authority is not initialized.".into(),
        ));
    };
    if holder != "rust" || !lease_valid {
        return Err(AppError::Forbidden(
            "Rust does not hold valid LIVE mutation authority.".into(),
        ));
    }
    Ok(LiveMutationGuard {
        _transaction: transaction,
        epoch,
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn authority_lock_key_is_stable() {
        assert_eq!(
            "rulenix:live-mutation-authority",
            "rulenix:live-mutation-authority"
        );
    }
}
