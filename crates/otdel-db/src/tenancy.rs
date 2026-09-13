//! Bureau-scoped transactions.
//!
//! Row-level security policies compare `bureau_id` with
//! `otdel.current_bureau_id()`, which reads the `otdel.bureau_id` run-time parameter.
//! That parameter is set here with `set_config(..., is_local => true)`, i.e. it is bound
//! to the transaction and is rolled back when the transaction ends — a pooled connection
//! handed to the next request never carries the previous request's context.
//!
//! The bureau id comes from the server-side session record, never from a request header
//! or body, so a client cannot choose which tenant it is scoped to.

use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::error::DbResult;

pub struct ScopedTx {
    tx: Transaction<'static, Postgres>,
    bureau_id: Uuid,
}

impl ScopedTx {
    pub(crate) async fn begin(pool: &PgPool, bureau_id: Uuid) -> DbResult<Self> {
        let mut tx = pool.begin().await?;
        sqlx::query("SELECT set_config('otdel.bureau_id', $1::text, true)")
            .bind(bureau_id)
            .execute(&mut *tx)
            .await?;
        Ok(Self { tx, bureau_id })
    }

    pub fn bureau_id(&self) -> Uuid {
        self.bureau_id
    }

    /// Connection handle for repository functions.
    pub fn conn(&mut self) -> &mut sqlx::PgConnection {
        &mut self.tx
    }

    pub async fn commit(self) -> DbResult<()> {
        self.tx.commit().await?;
        Ok(())
    }

    /// Explicit rollback. Dropping the transaction also rolls back, this is for
    /// readability where the rollback is a deliberate step.
    pub async fn rollback(self) -> DbResult<()> {
        self.tx.rollback().await?;
        Ok(())
    }
}
