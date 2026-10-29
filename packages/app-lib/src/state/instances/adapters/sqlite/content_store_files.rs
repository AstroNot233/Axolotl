//! Which content-store bytes each instance path is made of.

use sqlx::{Sqlite, Transaction};

/// Forgets the store rows of `paths` under `instance_id`.
pub(crate) async fn remove_instance_files(
    tx: &mut Transaction<'_, Sqlite>,
    instance_id: &str,
    paths: &[&str],
) -> crate::Result<()> {
    for path in paths {
        sqlx::query(
            "DELETE FROM store_instance_files WHERE instance_id = ? AND relative_path = ?",
        )
        .bind(instance_id)
        .bind(path)
        .execute(&mut **tx)
        .await?;
    }

    Ok(())
}
