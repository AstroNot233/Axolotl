//! Whether one instance takes part in one sync feature.

use sqlx::SqliteConnection;

use super::Collection;

/// The preferences, one per instance and feature.
pub(crate) struct InstanceSyncPreferences;

/// A preference is identified by the instance and the feature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InstancePreferenceKey {
    pub instance_id: String,
    pub feature: String,
}

/// What a preference carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct InstancePreferenceRecord {
    pub enabled: bool,
}

impl Collection for InstanceSyncPreferences {
    type Key = InstancePreferenceKey;
    type Scope = String;
    type Record = InstancePreferenceRecord;

    fn in_scope(scope: &Self::Scope, key: &Self::Key) -> bool {
        key.instance_id == *scope
    }

    async fn read(
        conn: &mut SqliteConnection,
        key: &Self::Key,
    ) -> crate::Result<Option<Self::Record>> {
        let row: Option<bool> = sqlx::query_scalar(
            "
            SELECT enabled
            FROM instance_sync_preferences
            WHERE instance_id = ? AND feature = ?
            ",
        )
        .bind(&key.instance_id)
        .bind(&key.feature)
        .fetch_optional(&mut *conn)
        .await?;

        Ok(row.map(|enabled| Self::Record { enabled }))
    }

    async fn write(
        conn: &mut SqliteConnection,
        key: &Self::Key,
        record: Option<&Self::Record>,
    ) -> crate::Result<()> {
        let Some(record) = record else {
            sqlx::query(
                "DELETE FROM instance_sync_preferences
                 WHERE instance_id = ? AND feature = ?",
            )
            .bind(&key.instance_id)
            .bind(&key.feature)
            .execute(&mut *conn)
            .await?;

            return Ok(());
        };

        write_row(conn, key, record).await
    }

    async fn read_all(
        conn: &mut SqliteConnection,
    ) -> crate::Result<Vec<(Self::Key, Self::Record)>> {
        let rows: Vec<(String, String, bool)> = sqlx::query_as(
            "
            SELECT instance_id, feature, enabled
            FROM instance_sync_preferences
            ",
        )
        .fetch_all(&mut *conn)
        .await?;

        Ok(rows
            .into_iter()
            .map(|(instance_id, feature, enabled)| {
                (
                    InstancePreferenceKey {
                        instance_id,
                        feature,
                    },
                    InstancePreferenceRecord { enabled },
                )
            })
            .collect())
    }

    async fn replace_all(
        conn: &mut SqliteConnection,
        records: &[(Self::Key, Self::Record)],
    ) -> crate::Result<()> {
        sqlx::query("DELETE FROM instance_sync_preferences")
            .execute(&mut *conn)
            .await?;
        for (key, record) in records {
            write_row(conn, key, record).await?;
        }
        Ok(())
    }

    async fn read_scoped(
        conn: &mut SqliteConnection,
        scope: &Self::Scope,
    ) -> crate::Result<Vec<(Self::Key, Self::Record)>> {
        let rows: Vec<(String, bool)> = sqlx::query_as(
            "
            SELECT feature, enabled
            FROM instance_sync_preferences
            WHERE instance_id = ?
            ",
        )
        .bind(scope)
        .fetch_all(&mut *conn)
        .await?;

        Ok(rows
            .into_iter()
            .map(|(feature, enabled)| {
                (
                    InstancePreferenceKey {
                        instance_id: scope.clone(),
                        feature,
                    },
                    InstancePreferenceRecord { enabled },
                )
            })
            .collect())
    }

    async fn replace_scoped(
        conn: &mut SqliteConnection,
        scope: &Self::Scope,
        records: &[(Self::Key, Self::Record)],
    ) -> crate::Result<()> {
        sqlx::query(
            "DELETE FROM instance_sync_preferences WHERE instance_id = ?",
        )
        .bind(scope)
        .execute(&mut *conn)
        .await?;
        for (key, record) in records {
            write_row(conn, key, record).await?;
        }
        Ok(())
    }
}

async fn write_row(
    conn: &mut SqliteConnection,
    key: &InstancePreferenceKey,
    record: &InstancePreferenceRecord,
) -> crate::Result<()> {
    sqlx::query(
        "
        INSERT INTO instance_sync_preferences (instance_id, feature, enabled)
        VALUES (?, ?, ?)
        ON CONFLICT (instance_id, feature) DO UPDATE SET
            enabled = excluded.enabled
        ",
    )
    .bind(&key.instance_id)
    .bind(&key.feature)
    .bind(record.enabled)
    .execute(&mut *conn)
    .await?;

    Ok(())
}
