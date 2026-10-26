//! The content a person keeps for later.

use sqlx::SqliteConnection;

use super::{Collection, Handle};
use crate::state::content_types::{
    ContentFavoriteProvider, ContentFavoriteType,
};

/// The favorites, one per provider project.
pub(crate) struct ContentFavorites;

/// A favorite is identified by the provider project it points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContentFavoriteKey {
    pub provider: ContentFavoriteProvider,
    pub project_id: String,
}

/// What a favorite carries besides the project it points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ContentFavoriteRecord {
    pub content_type: ContentFavoriteType,
    pub saved_at: i64,
}

impl Collection for ContentFavorites {
    type Key = ContentFavoriteKey;
    type Scope = ContentFavoriteKey;
    type Record = ContentFavoriteRecord;

    fn in_scope(scope: &Self::Scope, key: &Self::Key) -> bool {
        scope == key
    }

    async fn read(
        conn: &mut SqliteConnection,
        key: &Self::Key,
    ) -> crate::Result<Option<Self::Record>> {
        let row: Option<(String, i64)> = sqlx::query_as(
            "SELECT content_type, saved_at
             FROM content_favorites
             WHERE provider = ? AND project_id = ?",
        )
        .bind(key.provider.as_str())
        .bind(&key.project_id)
        .fetch_optional(&mut *conn)
        .await?;

        Ok(row.and_then(|(content_type, saved_at)| {
            Some(Self::Record {
                content_type: to_type(&content_type)?,
                saved_at,
            })
        }))
    }

    async fn write(
        conn: &mut SqliteConnection,
        key: &Self::Key,
        record: Option<&Self::Record>,
    ) -> crate::Result<()> {
        let Some(record) = record else {
            sqlx::query(
                "DELETE FROM content_favorites
                 WHERE provider = ? AND project_id = ?",
            )
            .bind(key.provider.as_str())
            .bind(&key.project_id)
            .execute(&mut *conn)
            .await?;

            return Ok(());
        };

        write_row(conn, key, record).await
    }

    async fn read_all(
        conn: &mut SqliteConnection,
    ) -> crate::Result<Vec<(Self::Key, Self::Record)>> {
        let rows: Vec<(String, String, String, i64)> = sqlx::query_as(
            "SELECT provider, project_id, content_type, saved_at
             FROM content_favorites",
        )
        .fetch_all(&mut *conn)
        .await?;

        Ok(rows
            .into_iter()
            .filter_map(|(provider, project_id, content_type, saved_at)| {
                Some((
                    ContentFavoriteKey {
                        provider: to_provider(&provider)?,
                        project_id,
                    },
                    ContentFavoriteRecord {
                        content_type: to_type(&content_type)?,
                        saved_at,
                    },
                ))
            })
            .collect())
    }

    async fn replace_all(
        conn: &mut SqliteConnection,
        records: &[(Self::Key, Self::Record)],
    ) -> crate::Result<()> {
        sqlx::query("DELETE FROM content_favorites")
            .execute(&mut *conn)
            .await?;
        for (key, record) in records {
            insert_row(conn, key, record).await?;
        }
        Ok(())
    }

    async fn read_scoped(
        conn: &mut SqliteConnection,
        scope: &Self::Scope,
    ) -> crate::Result<Vec<(Self::Key, Self::Record)>> {
        Ok(Self::read(conn, scope)
            .await?
            .map(|record| vec![(scope.clone(), record)])
            .unwrap_or_default())
    }

    async fn replace_scoped(
        conn: &mut SqliteConnection,
        scope: &Self::Scope,
        records: &[(Self::Key, Self::Record)],
    ) -> crate::Result<()> {
        sqlx::query(
            "DELETE FROM content_favorites WHERE provider = ? AND project_id = ?",
        )
        .bind(scope.provider.as_str())
        .bind(&scope.project_id)
        .execute(&mut *conn)
        .await?;
        for (key, record) in records {
            insert_row(conn, key, record).await?;
        }
        Ok(())
    }
}

async fn write_row(
    conn: &mut SqliteConnection,
    key: &ContentFavoriteKey,
    record: &ContentFavoriteRecord,
) -> crate::Result<()> {
    sqlx::query(
        "
        INSERT INTO content_favorites
            (provider, project_id, content_type, saved_at)
        VALUES (?, ?, ?, ?)
        ON CONFLICT (provider, project_id) DO UPDATE SET
            content_type = excluded.content_type
        ",
    )
    .bind(key.provider.as_str())
    .bind(&key.project_id)
    .bind(record.content_type.as_str())
    .bind(record.saved_at)
    .execute(&mut *conn)
    .await?;

    Ok(())
}

/// A replace writes the moment it was given, so a batch carries its own times.
async fn insert_row(
    conn: &mut SqliteConnection,
    key: &ContentFavoriteKey,
    record: &ContentFavoriteRecord,
) -> crate::Result<()> {
    sqlx::query(
        "
        INSERT INTO content_favorites
            (provider, project_id, content_type, saved_at)
        VALUES (?, ?, ?, ?)
        ",
    )
    .bind(key.provider.as_str())
    .bind(&key.project_id)
    .bind(record.content_type.as_str())
    .bind(record.saved_at)
    .execute(&mut *conn)
    .await?;

    Ok(())
}

/// The provider a stored favorite names, or nothing when this build does not
/// know it: a favorite is only useful while its identity can be read.
fn to_provider(provider: &str) -> Option<ContentFavoriteProvider> {
    ContentFavoriteProvider::parse(provider)
        .map_err(|error| {
            tracing::warn!(
                %provider,
                %error,
                "Skipping a favorite whose provider this build does not know"
            );
            error
        })
        .ok()
}

fn to_type(content_type: &str) -> Option<ContentFavoriteType> {
    ContentFavoriteType::parse(content_type)
        .map_err(|error| {
            tracing::warn!(
                %content_type,
                %error,
                "Skipping a favorite whose content type this build does not know"
            );
            error
        })
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn handle() -> Handle {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();
        Handle::new(pool)
    }

    fn key(
        provider: ContentFavoriteProvider,
        project_id: &str,
    ) -> ContentFavoriteKey {
        ContentFavoriteKey {
            provider,
            project_id: project_id.to_string(),
        }
    }

    #[tokio::test]
    async fn a_favorite_round_trips_and_removes() {
        let handle = handle().await;
        let favorite = key(ContentFavoriteProvider::Modrinth, "sodium");
        let record = ContentFavoriteRecord {
            content_type: ContentFavoriteType::Mod,
            saved_at: 1_700_000_000_000,
        };

        ContentFavorites::set(&handle, &favorite, Some(&record))
            .await
            .unwrap();
        assert_eq!(
            ContentFavorites::get(&handle, &favorite).await.unwrap(),
            Some(record)
        );

        ContentFavorites::set(&handle, &favorite, None)
            .await
            .unwrap();
        assert_eq!(
            ContentFavorites::get(&handle, &favorite).await.unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn saving_a_favorite_again_keeps_the_moment_it_was_first_saved() {
        let handle = handle().await;
        let favorite = key(ContentFavoriteProvider::Modrinth, "sodium");

        ContentFavorites::set(
            &handle,
            &favorite,
            Some(&ContentFavoriteRecord {
                content_type: ContentFavoriteType::Mod,
                saved_at: 1,
            }),
        )
        .await
        .unwrap();
        ContentFavorites::set(
            &handle,
            &favorite,
            Some(&ContentFavoriteRecord {
                content_type: ContentFavoriteType::Shader,
                saved_at: 2,
            }),
        )
        .await
        .unwrap();

        assert_eq!(
            ContentFavorites::get(&handle, &favorite).await.unwrap(),
            Some(ContentFavoriteRecord {
                content_type: ContentFavoriteType::Shader,
                saved_at: 1,
            })
        );
    }

    #[tokio::test]
    async fn a_whole_collection_round_trips() {
        let handle = handle().await;
        let records = vec![
            (
                key(ContentFavoriteProvider::Modrinth, "sodium"),
                ContentFavoriteRecord {
                    content_type: ContentFavoriteType::Mod,
                    saved_at: 2,
                },
            ),
            (
                key(ContentFavoriteProvider::Curseforge, "sapphire"),
                ContentFavoriteRecord {
                    content_type: ContentFavoriteType::Shader,
                    saved_at: 1,
                },
            ),
        ];

        ContentFavorites::set_all(&handle, &records).await.unwrap();

        let mut stored = ContentFavorites::get_all(&handle).await.unwrap();
        stored
            .sort_by(|left, right| left.0.project_id.cmp(&right.0.project_id));
        let mut expected = records;
        expected
            .sort_by(|left, right| left.0.project_id.cmp(&right.0.project_id));
        assert_eq!(stored, expected);
    }

    #[tokio::test]
    async fn the_collection_rejects_values_it_does_not_know() {
        let handle = handle().await;

        assert!(
            sqlx::query(
                "INSERT INTO content_favorites
                    (provider, project_id, content_type, saved_at)
                 VALUES ('unknown', 'bad-provider', 'mod', 3)",
            )
            .execute(handle.pool())
            .await
            .is_err()
        );
        assert!(
            sqlx::query(
                "INSERT INTO content_favorites
                    (provider, project_id, content_type, saved_at)
                 VALUES ('modrinth', 'bad-type', 'modpack', 3)",
            )
            .execute(handle.pool())
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn a_favorite_whose_identity_cannot_be_read_is_skipped() {
        let handle = handle().await;
        ContentFavorites::set(
            &handle,
            &key(ContentFavoriteProvider::Modrinth, "sodium"),
            Some(&ContentFavoriteRecord {
                content_type: ContentFavoriteType::Mod,
                saved_at: 1,
            }),
        )
        .await
        .unwrap();
        // The table refuses these values, so they are forced in to stand for a
        // database a later build may have left behind.
        sqlx::raw_sql(
            "PRAGMA ignore_check_constraints = ON;
             INSERT INTO content_favorites
                (provider, project_id, content_type, saved_at)
             VALUES ('unknown', 'bad-provider', 'mod', 3),
                ('modrinth', 'bad-type', 'modpack', 4);
             PRAGMA ignore_check_constraints = OFF;",
        )
        .execute(handle.pool())
        .await
        .unwrap();

        let stored = ContentFavorites::get_all(&handle).await.unwrap();

        assert_eq!(
            stored,
            vec![(
                key(ContentFavoriteProvider::Modrinth, "sodium"),
                ContentFavoriteRecord {
                    content_type: ContentFavoriteType::Mod,
                    saved_at: 1,
                },
            )]
        );
    }
}

#[cfg(test)]
mod replace_tests {
    use super::*;

    async fn handle() -> Handle {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();
        Handle::new(pool)
    }

    fn key(project_id: &str) -> ContentFavoriteKey {
        ContentFavoriteKey {
            provider: ContentFavoriteProvider::Modrinth,
            project_id: project_id.to_string(),
        }
    }

    fn record(saved_at: i64) -> ContentFavoriteRecord {
        ContentFavoriteRecord {
            content_type: ContentFavoriteType::Mod,
            saved_at,
        }
    }

    #[tokio::test]
    async fn a_batch_keeps_the_time_it_was_given() {
        let handle = handle().await;

        ContentFavorites::set_all(&handle, &[(key("sodium"), record(7))])
            .await
            .unwrap();

        assert_eq!(
            ContentFavorites::get(&handle, &key("sodium"))
                .await
                .unwrap(),
            Some(record(7))
        );
    }

    #[tokio::test]
    async fn a_batch_refuses_the_same_key_twice() {
        let handle = handle().await;

        assert!(
            ContentFavorites::set_all(
                &handle,
                &[(key("sodium"), record(1)), (key("sodium"), record(2))],
            )
            .await
            .is_err()
        );
        assert!(ContentFavorites::get_all(&handle).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_scoped_replace_refuses_a_key_outside_its_scope() {
        let handle = handle().await;

        assert!(
            ContentFavorites::set_scoped(
                &handle,
                &key("sodium"),
                &[(key("lithium"), record(1))],
            )
            .await
            .is_err()
        );
        assert!(
            ContentFavorites::get(&handle, &key("lithium"))
                .await
                .unwrap()
                .is_none()
        );
    }
}
