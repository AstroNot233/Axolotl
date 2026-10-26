//! The `options.txt` an instance had before a modpack update replaced it.

use super::{Collection, Handle};

/// The update state, one per instance.
pub(crate) struct GameOptionUpdateState;

/// An instance's update state is identified by the instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UpdateStateKey {
    pub instance_id: String,
}

/// What the instance's `options.txt` looked like before the update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UpdateStateRecord {
    pub had_file: bool,
    pub sha1: Option<String>,
    pub document: Option<Vec<u8>>,
}

impl Collection for GameOptionUpdateState {
    type Key = UpdateStateKey;
    type Record = UpdateStateRecord;

    async fn set(
        handle: &Handle,
        key: &Self::Key,
        record: Option<&Self::Record>,
    ) -> crate::Result<()> {
        let Some(record) = record else {
            sqlx::query(
                "DELETE FROM instance_game_option_update_state
                 WHERE instance_id = ?",
            )
            .bind(&key.instance_id)
            .execute(handle.pool())
            .await?;

            return Ok(());
        };

        sqlx::query(
            "
            INSERT INTO instance_game_option_update_state
                (instance_id, had_file, sha1, document)
            VALUES (?, ?, ?, ?)
            ON CONFLICT (instance_id) DO UPDATE SET
                had_file = excluded.had_file,
                sha1 = excluded.sha1,
                document = excluded.document
            ",
        )
        .bind(&key.instance_id)
        .bind(record.had_file)
        .bind(&record.sha1)
        .bind(&record.document)
        .execute(handle.pool())
        .await?;

        Ok(())
    }

    async fn get(
        handle: &Handle,
        key: &Self::Key,
    ) -> crate::Result<Option<Self::Record>> {
        let row: Option<(bool, Option<String>, Option<Vec<u8>>)> =
            sqlx::query_as(
                "
                SELECT had_file, sha1, document
                FROM instance_game_option_update_state
                WHERE instance_id = ?
                ",
            )
            .bind(&key.instance_id)
            .fetch_optional(handle.pool())
            .await?;

        Ok(row.map(|(had_file, sha1, document)| Self::Record {
            had_file,
            sha1,
            document,
        }))
    }

    async fn set_all(
        handle: &Handle,
        records: &[(Self::Key, Self::Record)],
    ) -> crate::Result<()> {
        let mut transaction = handle.pool().begin().await?;
        // The call replaces the collection, so a key the caller leaves out is
        // gone rather than left behind for a later read to return.
        sqlx::query("DELETE FROM instance_game_option_update_state")
            .execute(&mut *transaction)
            .await?;
        for (key, record) in records {
            sqlx::query(
                "
                INSERT INTO instance_game_option_update_state
                    (instance_id, had_file, sha1, document)
                VALUES (?, ?, ?, ?)
                ",
            )
            .bind(&key.instance_id)
            .bind(record.had_file)
            .bind(&record.sha1)
            .bind(&record.document)
            .execute(&mut *transaction)
            .await?;
        }
        transaction.commit().await?;

        Ok(())
    }

    async fn get_all(
        handle: &Handle,
    ) -> crate::Result<Vec<(Self::Key, Self::Record)>> {
        let rows: Vec<(String, bool, Option<String>, Option<Vec<u8>>)> =
            sqlx::query_as(
                "
                SELECT instance_id, had_file, sha1, document
                FROM instance_game_option_update_state
                ",
            )
            .fetch_all(handle.pool())
            .await?;

        Ok(rows
            .into_iter()
            .map(|(instance_id, had_file, sha1, document)| {
                (
                    UpdateStateKey { instance_id },
                    UpdateStateRecord {
                        had_file,
                        sha1,
                        document,
                    },
                )
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        Handle::new(pool)
    }

    fn key(instance_id: &str) -> UpdateStateKey {
        UpdateStateKey {
            instance_id: instance_id.to_string(),
        }
    }

    #[tokio::test]
    async fn an_update_state_round_trips_and_clears() {
        let handle = handle("instance").await;
        let record = UpdateStateRecord {
            had_file: true,
            sha1: Some("abc".to_string()),
            document: Some(b"options".to_vec()),
        };

        GameOptionUpdateState::set(&handle, &key("instance"), Some(&record))
            .await
            .unwrap();
        assert_eq!(
            GameOptionUpdateState::get(&handle, &key("instance"))
                .await
                .unwrap(),
            Some(record)
        );
        assert_eq!(
            GameOptionUpdateState::get_all(&handle).await.unwrap().len(),
            1
        );

        GameOptionUpdateState::set(&handle, &key("instance"), None)
            .await
            .unwrap();
        assert_eq!(
            GameOptionUpdateState::get(&handle, &key("instance"))
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn writing_a_whole_collection_drops_what_it_leaves_out() {
        let handle = handle("kept").await;
        sqlx::query(
            "INSERT INTO instances
                (id, path, install_stage, launcher_feature_version,
                 name, created, modified)
             VALUES ('dropped', 'dropped', 'installed', '1', 'dropped', 1, 1)",
        )
        .execute(handle.pool())
        .await
        .unwrap();
        let record = UpdateStateRecord {
            had_file: false,
            sha1: None,
            document: None,
        };
        GameOptionUpdateState::set(&handle, &key("kept"), Some(&record))
            .await
            .unwrap();
        GameOptionUpdateState::set(&handle, &key("dropped"), Some(&record))
            .await
            .unwrap();

        GameOptionUpdateState::set_all(
            &handle,
            &[(key("kept"), record.clone())],
        )
        .await
        .unwrap();

        assert_eq!(
            GameOptionUpdateState::get_all(&handle).await.unwrap(),
            vec![(key("kept"), record)]
        );
    }
}
