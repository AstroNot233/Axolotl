//! The checkpoint a synced option leaves while its write is in flight.

use sqlx::SqliteConnection;

use super::{Collection, Handle};

/// The checkpoints, one per instance, feature and variant.
pub(crate) struct InstanceSyncCheckpoints;

/// One checkpoint is identified by the instance, the feature and the variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CheckpointKey {
    pub instance_id: String,
    pub feature: String,
    pub variant: String,
}

/// Every checkpoint one instance keeps for one feature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CheckpointScope {
    pub instance_id: String,
    pub feature: String,
}

/// Whether the write behind a checkpoint has landed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CheckpointStatus {
    Pending,
    Ready,
}

impl CheckpointStatus {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Ready => "ready",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(Self::Pending),
            "ready" => Some(Self::Ready),
            _ => None,
        }
    }
}

/// What a checkpoint carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CheckpointRecord {
    pub expected_sha1: String,
    pub merge_base: Option<Vec<u8>>,
    pub source_revision: i64,
    pub status: CheckpointStatus,
    pub link_mode: Option<String>,
}

impl Collection for InstanceSyncCheckpoints {
    type Key = CheckpointKey;
    type Scope = CheckpointScope;
    type Record = CheckpointRecord;

    fn in_scope(scope: &Self::Scope, key: &Self::Key) -> bool {
        scope.instance_id == key.instance_id && scope.feature == key.feature
    }

    async fn read(
        conn: &mut SqliteConnection,
        key: &Self::Key,
    ) -> crate::Result<Option<Self::Record>> {
        let row: Option<(
            String,
            Option<Vec<u8>>,
            i64,
            String,
            Option<String>,
        )> = sqlx::query_as(
            "
                SELECT expected_sha1, merge_base, source_revision, status,
                    link_mode
                FROM instance_sync_checkpoints
                WHERE instance_id = ? AND feature = ? AND variant = ?
                ",
        )
        .bind(&key.instance_id)
        .bind(&key.feature)
        .bind(&key.variant)
        .fetch_optional(&mut *conn)
        .await?;

        Ok(row.and_then(
            |(
                expected_sha1,
                merge_base,
                source_revision,
                status,
                link_mode,
            )| {
                Some(Self::Record {
                    expected_sha1,
                    merge_base,
                    source_revision,
                    status: to_status(&status)?,
                    link_mode,
                })
            },
        ))
    }

    async fn write(
        conn: &mut SqliteConnection,
        key: &Self::Key,
        record: Option<&Self::Record>,
    ) -> crate::Result<()> {
        let Some(record) = record else {
            sqlx::query(
                "DELETE FROM instance_sync_checkpoints
                 WHERE instance_id = ? AND feature = ? AND variant = ?",
            )
            .bind(&key.instance_id)
            .bind(&key.feature)
            .bind(&key.variant)
            .execute(&mut *conn)
            .await?;

            return Ok(());
        };

        write_row(conn, key, record).await
    }

    async fn read_all(
        conn: &mut SqliteConnection,
    ) -> crate::Result<Vec<(Self::Key, Self::Record)>> {
        let rows: Vec<(
            String,
            String,
            String,
            String,
            Option<Vec<u8>>,
            i64,
            String,
            Option<String>,
        )> = sqlx::query_as(
            "
            SELECT instance_id, feature, variant, expected_sha1, merge_base,
                source_revision, status, link_mode
            FROM instance_sync_checkpoints
            ",
        )
        .fetch_all(&mut *conn)
        .await?;

        Ok(rows
            .into_iter()
            .filter_map(
                |(
                    instance_id,
                    feature,
                    variant,
                    expected_sha1,
                    merge_base,
                    source_revision,
                    status,
                    link_mode,
                )| {
                    Some((
                        CheckpointKey {
                            instance_id,
                            feature,
                            variant,
                        },
                        CheckpointRecord {
                            expected_sha1,
                            merge_base,
                            source_revision,
                            status: to_status(&status)?,
                            link_mode,
                        },
                    ))
                },
            )
            .collect())
    }

    async fn replace_all(
        conn: &mut SqliteConnection,
        records: &[(Self::Key, Self::Record)],
    ) -> crate::Result<()> {
        sqlx::query("DELETE FROM instance_sync_checkpoints")
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
        let rows: Vec<(
            String,
            String,
            Option<Vec<u8>>,
            i64,
            String,
            Option<String>,
        )> = sqlx::query_as(
            "
            SELECT variant, expected_sha1, merge_base, source_revision,
                status, link_mode
            FROM instance_sync_checkpoints
            WHERE instance_id = ? AND feature = ?
            ",
        )
        .bind(&scope.instance_id)
        .bind(&scope.feature)
        .fetch_all(&mut *conn)
        .await?;

        Ok(rows
            .into_iter()
            .filter_map(
                |(
                    variant,
                    expected_sha1,
                    merge_base,
                    source_revision,
                    status,
                    link_mode,
                )| {
                    Some((
                        CheckpointKey {
                            instance_id: scope.instance_id.clone(),
                            feature: scope.feature.clone(),
                            variant,
                        },
                        CheckpointRecord {
                            expected_sha1,
                            merge_base,
                            source_revision,
                            status: to_status(&status)?,
                            link_mode,
                        },
                    ))
                },
            )
            .collect())
    }

    async fn replace_scoped(
        conn: &mut SqliteConnection,
        scope: &Self::Scope,
        records: &[(Self::Key, Self::Record)],
    ) -> crate::Result<()> {
        sqlx::query(
            "DELETE FROM instance_sync_checkpoints
             WHERE instance_id = ? AND feature = ?",
        )
        .bind(&scope.instance_id)
        .bind(&scope.feature)
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
    key: &CheckpointKey,
    record: &CheckpointRecord,
) -> crate::Result<()> {
    sqlx::query(
        "
        INSERT INTO instance_sync_checkpoints
            (instance_id, feature, variant, expected_sha1, merge_base,
             source_revision, status, link_mode)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT (instance_id, feature, variant) DO UPDATE SET
            expected_sha1 = excluded.expected_sha1,
            merge_base = excluded.merge_base,
            source_revision = excluded.source_revision,
            status = excluded.status,
            link_mode = excluded.link_mode
        ",
    )
    .bind(&key.instance_id)
    .bind(&key.feature)
    .bind(&key.variant)
    .bind(&record.expected_sha1)
    .bind(&record.merge_base)
    .bind(record.source_revision)
    .bind(record.status.as_str())
    .bind(&record.link_mode)
    .execute(&mut *conn)
    .await?;

    Ok(())
}

fn to_status(status: &str) -> Option<CheckpointStatus> {
    CheckpointStatus::parse(status).or_else(|| {
        tracing::warn!(
            %status,
            "Skipping a sync checkpoint whose status this build does not know"
        );
        None
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::store::atom;

    async fn handle(instance: &str) -> Handle {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO instances
                (id, path, install_stage, launcher_feature_version,
                 name, created, modified)
             VALUES (?, ?, 'installed', '1', ?, 1, 1)",
        )
        .bind(instance)
        .bind(instance)
        .bind(instance)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO instance_sync_preferences (instance_id, feature, enabled)
             VALUES (?, 'game_options', 1)",
        )
        .bind(instance)
        .execute(&pool)
        .await
        .unwrap();
        Handle::new(pool)
    }

    fn key(variant: &str) -> CheckpointKey {
        CheckpointKey {
            instance_id: "instance".to_string(),
            feature: "game_options".to_string(),
            variant: variant.to_string(),
        }
    }

    fn pending() -> CheckpointRecord {
        CheckpointRecord {
            expected_sha1: "abc".to_string(),
            merge_base: None,
            source_revision: 1,
            status: CheckpointStatus::Pending,
            link_mode: None,
        }
    }

    #[tokio::test]
    async fn a_checkpoint_finishes_inside_one_unit() {
        let handle = handle("instance").await;
        let checkpoint = key("default");
        InstanceSyncCheckpoints::set(&handle, &checkpoint, Some(&pending()))
            .await
            .unwrap();

        let mut unit = atom(&handle).await.unwrap();
        let mut record =
            InstanceSyncCheckpoints::atom_get(&mut unit, &checkpoint)
                .await
                .unwrap()
                .expect("checkpoint");
        record.status = CheckpointStatus::Ready;
        record.link_mode = Some("hard".to_string());
        InstanceSyncCheckpoints::atom_set(
            &mut unit,
            &checkpoint,
            Some(&record),
        )
        .await
        .unwrap();
        unit.commit().await.unwrap();

        let stored = InstanceSyncCheckpoints::get(&handle, &checkpoint)
            .await
            .unwrap()
            .expect("checkpoint");
        assert_eq!(stored.status, CheckpointStatus::Ready);
        assert_eq!(stored.link_mode.as_deref(), Some("hard"));
    }

    #[tokio::test]
    async fn a_scope_clears_its_own_feature_only() {
        let handle = handle("instance").await;
        let scope = CheckpointScope {
            instance_id: "instance".to_string(),
            feature: "game_options".to_string(),
        };
        InstanceSyncCheckpoints::set(
            &handle,
            &key("default"),
            Some(&pending()),
        )
        .await
        .unwrap();
        InstanceSyncCheckpoints::set(&handle, &key("legacy"), Some(&pending()))
            .await
            .unwrap();

        InstanceSyncCheckpoints::set_scoped(&handle, &scope, &[])
            .await
            .unwrap();

        assert!(
            InstanceSyncCheckpoints::get_all(&handle)
                .await
                .unwrap()
                .is_empty()
        );
    }
}
