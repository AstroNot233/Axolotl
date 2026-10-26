//! Which resource a provider project is stored as.

use sqlx::SqliteConnection;

use super::{Collection, Handle};

/// The provider projects that have a resource of their own.
pub(crate) struct ResourceSourceProjects;

/// A project is identified by the provider it comes from and its own ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResourceSourceProjectKey {
    pub source: String,
    pub project_id: String,
}

/// What the project maps to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResourceSourceProjectRecord {
    pub resource_id: String,
}

impl Collection for ResourceSourceProjects {
    type Key = ResourceSourceProjectKey;
    type Scope = ResourceSourceProjectKey;
    type Record = ResourceSourceProjectRecord;

    fn in_scope(scope: &Self::Scope, key: &Self::Key) -> bool {
        scope == key
    }

    async fn read(
        conn: &mut SqliteConnection,
        key: &Self::Key,
    ) -> crate::Result<Option<Self::Record>> {
        let resource_id: Option<String> = sqlx::query_scalar(
            "SELECT resource_id
             FROM resource_source_projects
             WHERE source = ? AND project_id = ?",
        )
        .bind(&key.source)
        .bind(&key.project_id)
        .fetch_optional(&mut *conn)
        .await?;

        Ok(resource_id.map(|resource_id| Self::Record { resource_id }))
    }

    async fn write(
        conn: &mut SqliteConnection,
        key: &Self::Key,
        record: Option<&Self::Record>,
    ) -> crate::Result<()> {
        let Some(record) = record else {
            sqlx::query(
                "DELETE FROM resource_source_projects
                 WHERE source = ? AND project_id = ?",
            )
            .bind(&key.source)
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
        let rows: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT source, project_id, resource_id
             FROM resource_source_projects",
        )
        .fetch_all(&mut *conn)
        .await?;

        Ok(rows
            .into_iter()
            .map(|(source, project_id, resource_id)| {
                (
                    ResourceSourceProjectKey { source, project_id },
                    ResourceSourceProjectRecord { resource_id },
                )
            })
            .collect())
    }

    async fn replace_all(
        conn: &mut SqliteConnection,
        records: &[(Self::Key, Self::Record)],
    ) -> crate::Result<()> {
        sqlx::query("DELETE FROM resource_source_projects")
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
        Self::write(conn, scope, records.first().map(|(_, record)| record))
            .await
    }
}

async fn write_row(
    conn: &mut SqliteConnection,
    key: &ResourceSourceProjectKey,
    record: &ResourceSourceProjectRecord,
) -> crate::Result<()> {
    sqlx::query(
        "
        INSERT INTO resource_source_projects
            (source, project_id, resource_id)
        VALUES (?, ?, ?)
        ON CONFLICT (source, project_id) DO UPDATE SET
            resource_id = excluded.resource_id
        ",
    )
    .bind(&key.source)
    .bind(&key.project_id)
    .bind(&record.resource_id)
    .execute(&mut *conn)
    .await?;

    Ok(())
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

    async fn resource(handle: &Handle, resource_id: &str) {
        sqlx::query("INSERT INTO resources (id) VALUES (?)")
            .bind(resource_id)
            .execute(handle.pool())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_mapping_round_trips_and_removes() {
        let handle = handle().await;
        resource(&handle, "resource").await;
        let project = ResourceSourceProjectKey {
            source: "modrinth".to_string(),
            project_id: "sodium".to_string(),
        };
        let record = ResourceSourceProjectRecord {
            resource_id: "resource".to_string(),
        };

        ResourceSourceProjects::set(&handle, &project, Some(&record))
            .await
            .unwrap();
        assert_eq!(
            ResourceSourceProjects::get(&handle, &project)
                .await
                .unwrap(),
            Some(record)
        );

        ResourceSourceProjects::set(&handle, &project, None)
            .await
            .unwrap();
        assert_eq!(
            ResourceSourceProjects::get(&handle, &project)
                .await
                .unwrap(),
            None
        );
        assert!(
            ResourceSourceProjects::get_all(&handle)
                .await
                .unwrap()
                .is_empty()
        );
    }
}

#[cfg(test)]
mod composition_tests {
    //! The favorites read resolves their resource through this collection, so
    //! the two are exercised together here, where the fixtures may seed rows
    //! the collections do not own yet.

    use super::*;
    use crate::state::content_favorites::{
        ContentFavoriteInput, ContentFavoriteProvider, ContentFavoriteType,
        add, list,
    };

    async fn handle() -> Handle {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();
        sqlx::raw_sql(
            "INSERT INTO resources (id) VALUES ('resource');
             INSERT INTO resource_source_projects
                (source, project_id, resource_id)
             VALUES ('modrinth', 'sodium', 'resource');",
        )
        .execute(&pool)
        .await
        .unwrap();
        Handle::new(pool)
    }

    #[tokio::test]
    async fn a_favorite_carries_the_resource_its_project_maps_to() {
        let handle = handle().await;

        let favorite = add(
            ContentFavoriteInput {
                provider: ContentFavoriteProvider::Modrinth,
                project_id: "sodium".to_string(),
                content_type: ContentFavoriteType::Mod,
            },
            &handle,
        )
        .await
        .unwrap();

        assert_eq!(favorite.resource_id.as_deref(), Some("resource"));
        assert_eq!(
            list(&handle).await.unwrap()[0].resource_id.as_deref(),
            Some("resource")
        );
    }
}
