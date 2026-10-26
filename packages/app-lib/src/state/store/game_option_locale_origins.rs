//! Where a mod setting's label was found, per scope and option.

use sqlx::SqliteConnection;

use super::{Collection, Handle};

/// The origins, one per scope and option.
pub(crate) struct GameOptionLocaleOrigins;

/// An origin is identified by the scope it is known in and the option it
/// describes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LocaleOriginKey {
    pub scope: String,
    pub option_id: String,
}

/// What an origin carries beyond its key.
///
/// The observation and the resolved origin are opaque text: what one says is
/// the caller's business.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LocaleOriginRecord {
    pub source_instance_id: Option<String>,
    pub source_game_version: Option<String>,
    pub backfilled: bool,
    pub observation: Option<String>,
    pub origin: Option<String>,
}

impl Collection for GameOptionLocaleOrigins {
    type Key = LocaleOriginKey;
    type Scope = String;
    type Record = LocaleOriginRecord;

    fn in_scope(scope: &Self::Scope, key: &Self::Key) -> bool {
        key.scope == *scope
    }

    async fn read(
        conn: &mut SqliteConnection,
        key: &Self::Key,
    ) -> crate::Result<Option<Self::Record>> {
        let row: Option<(
            Option<String>,
            Option<String>,
            bool,
            Option<String>,
            Option<String>,
        )> = sqlx::query_as(
            "
            SELECT source_instance_id, source_game_version, backfilled,
                observation_json, origin_json
            FROM game_option_locale_origins
            WHERE scope = ? AND option_id = ?
            ",
        )
        .bind(&key.scope)
        .bind(&key.option_id)
        .fetch_optional(&mut *conn)
        .await?;

        Ok(row.map(
            |(
                source_instance_id,
                source_game_version,
                backfilled,
                observation,
                origin,
            )| Self::Record {
                source_instance_id,
                source_game_version,
                backfilled,
                observation,
                origin,
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
                "DELETE FROM game_option_locale_origins
                 WHERE scope = ? AND option_id = ?",
            )
            .bind(&key.scope)
            .bind(&key.option_id)
            .execute(&mut *conn)
            .await?;

            return Ok(());
        };

        write_row(conn, key, record).await
    }

    async fn read_all(
        conn: &mut SqliteConnection,
    ) -> crate::Result<Vec<(Self::Key, Self::Record)>> {
        let rows: Vec<StoredOriginRow> = sqlx::query_as(
            "
            SELECT scope, option_id, source_instance_id, source_game_version,
                backfilled, observation_json, origin_json
            FROM game_option_locale_origins
            ",
        )
        .fetch_all(&mut *conn)
        .await?;

        Ok(rows.into_iter().map(to_entry).collect())
    }

    async fn replace_all(
        conn: &mut SqliteConnection,
        records: &[(Self::Key, Self::Record)],
    ) -> crate::Result<()> {
        sqlx::query("DELETE FROM game_option_locale_origins")
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
        let rows: Vec<StoredOriginRow> = sqlx::query_as(
            "
            SELECT scope, option_id, source_instance_id, source_game_version,
                backfilled, observation_json, origin_json
            FROM game_option_locale_origins
            WHERE scope = ?
            ",
        )
        .bind(scope)
        .fetch_all(&mut *conn)
        .await?;

        Ok(rows.into_iter().map(to_entry).collect())
    }

    async fn replace_scoped(
        conn: &mut SqliteConnection,
        scope: &Self::Scope,
        records: &[(Self::Key, Self::Record)],
    ) -> crate::Result<()> {
        sqlx::query("DELETE FROM game_option_locale_origins WHERE scope = ?")
            .bind(scope)
            .execute(&mut *conn)
            .await?;
        for (key, record) in records {
            write_row(conn, key, record).await?;
        }
        Ok(())
    }
}

/// A stored origin as it is read back: the key first, then the record.
type StoredOriginRow = (
    String,
    String,
    Option<String>,
    Option<String>,
    bool,
    Option<String>,
    Option<String>,
);

fn to_entry(row: StoredOriginRow) -> (LocaleOriginKey, LocaleOriginRecord) {
    let (
        scope,
        option_id,
        source_instance_id,
        source_game_version,
        backfilled,
        observation,
        origin,
    ) = row;
    (
        LocaleOriginKey { scope, option_id },
        LocaleOriginRecord {
            source_instance_id,
            source_game_version,
            backfilled,
            observation,
            origin,
        },
    )
}

async fn write_row(
    conn: &mut SqliteConnection,
    key: &LocaleOriginKey,
    record: &LocaleOriginRecord,
) -> crate::Result<()> {
    sqlx::query(
        "
        INSERT INTO game_option_locale_origins
            (scope, option_id, source_instance_id, source_game_version,
             backfilled, observation_json, origin_json)
        VALUES (?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT (scope, option_id) DO UPDATE SET
            source_instance_id = excluded.source_instance_id,
            source_game_version = excluded.source_game_version,
            backfilled = excluded.backfilled,
            observation_json = excluded.observation_json,
            origin_json = excluded.origin_json
        ",
    )
    .bind(&key.scope)
    .bind(&key.option_id)
    .bind(&record.source_instance_id)
    .bind(&record.source_game_version)
    .bind(record.backfilled)
    .bind(&record.observation)
    .bind(&record.origin)
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

    fn key(scope: &str, option_id: &str) -> LocaleOriginKey {
        LocaleOriginKey {
            scope: scope.to_string(),
            option_id: option_id.to_string(),
        }
    }

    fn observed() -> LocaleOriginRecord {
        LocaleOriginRecord {
            source_instance_id: Some("instance".to_string()),
            source_game_version: Some("1.21".to_string()),
            backfilled: false,
            observation: Some("{\"snapshot\":\"one\"}".to_string()),
            origin: None,
        }
    }

    #[tokio::test]
    async fn an_origin_round_trips_and_clears() {
        let handle = handle().await;
        GameOptionLocaleOrigins::set(
            &handle,
            &key("", "external:key_one"),
            Some(&observed()),
        )
        .await
        .unwrap();

        assert_eq!(
            GameOptionLocaleOrigins::get(&handle, &key("", "external:key_one"))
                .await
                .unwrap(),
            Some(observed())
        );

        GameOptionLocaleOrigins::set(
            &handle,
            &key("", "external:key_one"),
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            GameOptionLocaleOrigins::get(&handle, &key("", "external:key_one"))
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn a_scope_reads_and_clears_only_its_own_origins() {
        let handle = handle().await;
        for (scope, option_id) in [
            ("", "external:key_one"),
            ("one", "external:key_two"),
            ("two", "external:key_three"),
        ] {
            GameOptionLocaleOrigins::set(
                &handle,
                &key(scope, option_id),
                Some(&observed()),
            )
            .await
            .unwrap();
        }

        let scoped =
            GameOptionLocaleOrigins::get_scoped(&handle, &"one".to_string())
                .await
                .unwrap();
        assert_eq!(scoped.len(), 1);
        assert_eq!(scoped[0].0.option_id, "external:key_two");

        GameOptionLocaleOrigins::set_scoped(&handle, &"one".to_string(), &[])
            .await
            .unwrap();
        assert_eq!(
            GameOptionLocaleOrigins::get_all(&handle)
                .await
                .unwrap()
                .len(),
            2
        );
    }

    #[tokio::test]
    async fn a_replacement_writes_exactly_what_it_was_given() {
        let handle = handle().await;
        GameOptionLocaleOrigins::set(
            &handle,
            &key("", "external:key_one"),
            Some(&observed()),
        )
        .await
        .unwrap();

        let replaced = key("", "external:key_two");
        GameOptionLocaleOrigins::set_all(
            &handle,
            &[(replaced.clone(), observed())],
        )
        .await
        .unwrap();

        assert_eq!(
            GameOptionLocaleOrigins::get(&handle, &key("", "external:key_one"))
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            GameOptionLocaleOrigins::get(&handle, &replaced)
                .await
                .unwrap(),
            Some(observed())
        );
    }

    #[tokio::test]
    async fn a_scoped_replacement_refuses_a_key_outside_its_scope() {
        let handle = handle().await;
        let error = GameOptionLocaleOrigins::set_scoped(
            &handle,
            &"one".to_string(),
            &[(key("two", "external:key_one"), observed())],
        )
        .await
        .unwrap_err();

        assert!(matches!(
            error.raw.as_ref(),
            crate::ErrorKind::InputError(_)
        ));
    }
}
