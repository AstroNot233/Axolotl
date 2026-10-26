//! The shared value of a game setting, one per option.

use sqlx::SqliteConnection;

use super::{Collection, Handle};

/// The values, one per option.
pub(crate) struct SyncedGameOptionValues;

/// What kind of setting an option is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GameOptionValueKind {
    Vanilla,
    External,
}

impl GameOptionValueKind {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Vanilla => "vanilla",
            Self::External => "external",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "vanilla" => Some(Self::Vanilla),
            "external" => Some(Self::External),
            _ => None,
        }
    }
}

/// Where a shared value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GameOptionValueOrigin {
    AppEditor,
    Instance,
    SourceSeed,
}

impl GameOptionValueOrigin {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::AppEditor => "app_editor",
            Self::Instance => "instance",
            Self::SourceSeed => "source_seed",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "app_editor" => Some(Self::AppEditor),
            "instance" => Some(Self::Instance),
            "source_seed" => Some(Self::SourceSeed),
            _ => None,
        }
    }
}

/// What a value carries beyond its key.
///
/// The canonical value is opaque text: what it says is the caller's business.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GameOptionValueRecord {
    pub kind: GameOptionValueKind,
    pub raw_key: Option<String>,
    pub canonical_type: String,
    pub canonical_value: Option<String>,
    pub value_codec: String,
    pub seeded: bool,
    pub revision: i64,
    pub origin: GameOptionValueOrigin,
    pub source_game_version: Option<String>,
    pub source_instance_id: Option<String>,
    pub updated_at: i64,
}

impl Collection for SyncedGameOptionValues {
    type Key = String;
    type Scope = String;
    type Record = GameOptionValueRecord;

    fn in_scope(scope: &Self::Scope, key: &Self::Key) -> bool {
        scope == key
    }

    async fn read(
        conn: &mut SqliteConnection,
        key: &Self::Key,
    ) -> crate::Result<Option<Self::Record>> {
        let row: Option<ValueRow> = sqlx::query_as(
            "
            SELECT kind, raw_key, canonical_type, canonical_value_json,
                value_codec, seeded, revision, origin, source_game_version,
                source_instance_id, updated_at
            FROM synced_game_option_values
            WHERE option_id = ?
            ",
        )
        .bind(key)
        .fetch_optional(&mut *conn)
        .await?;

        Ok(row.and_then(to_record))
    }

    async fn write(
        conn: &mut SqliteConnection,
        key: &Self::Key,
        record: Option<&Self::Record>,
    ) -> crate::Result<()> {
        let Some(record) = record else {
            sqlx::query(
                "DELETE FROM synced_game_option_values WHERE option_id = ?",
            )
            .bind(key)
            .execute(&mut *conn)
            .await?;

            return Ok(());
        };

        write_row(conn, key, record).await
    }

    async fn read_all(
        conn: &mut SqliteConnection,
    ) -> crate::Result<Vec<(Self::Key, Self::Record)>> {
        let rows: Vec<KeyedValueRow> = sqlx::query_as(
            "
            SELECT option_id, kind, raw_key, canonical_type,
                canonical_value_json, value_codec, seeded, revision, origin,
                source_game_version, source_instance_id, updated_at
            FROM synced_game_option_values
            ",
        )
        .fetch_all(&mut *conn)
        .await?;

        Ok(rows.into_iter().filter_map(to_entry).collect())
    }

    async fn replace_all(
        conn: &mut SqliteConnection,
        records: &[(Self::Key, Self::Record)],
    ) -> crate::Result<()> {
        sqlx::query("DELETE FROM synced_game_option_values")
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

/// A stored value as it is read back, without its key.
type ValueRow = (
    String,
    Option<String>,
    String,
    Option<String>,
    String,
    bool,
    i64,
    String,
    Option<String>,
    Option<String>,
    i64,
);

/// A stored value as it is read back, key first.
type KeyedValueRow = (
    String,
    String,
    Option<String>,
    String,
    Option<String>,
    String,
    bool,
    i64,
    String,
    Option<String>,
    Option<String>,
    i64,
);

fn to_entry(row: KeyedValueRow) -> Option<(String, GameOptionValueRecord)> {
    let (
        option_id,
        kind,
        raw_key,
        canonical_type,
        canonical_value,
        value_codec,
        seeded,
        revision,
        origin,
        source_game_version,
        source_instance_id,
        updated_at,
    ) = row;
    let record = to_record((
        kind,
        raw_key,
        canonical_type,
        canonical_value,
        value_codec,
        seeded,
        revision,
        origin,
        source_game_version,
        source_instance_id,
        updated_at,
    ))?;
    Some((option_id, record))
}

fn to_record(row: ValueRow) -> Option<GameOptionValueRecord> {
    let (
        kind,
        raw_key,
        canonical_type,
        canonical_value,
        value_codec,
        seeded,
        revision,
        origin,
        source_game_version,
        source_instance_id,
        updated_at,
    ) = row;
    Some(GameOptionValueRecord {
        kind: to_kind(&kind)?,
        raw_key,
        canonical_type,
        canonical_value,
        value_codec,
        seeded,
        revision,
        origin: to_origin(&origin)?,
        source_game_version,
        source_instance_id,
        updated_at,
    })
}

async fn write_row(
    conn: &mut SqliteConnection,
    key: &str,
    record: &GameOptionValueRecord,
) -> crate::Result<()> {
    sqlx::query(
        "
        INSERT INTO synced_game_option_values
            (option_id, kind, raw_key, canonical_type, canonical_value_json,
             value_codec, seeded, revision, origin, source_game_version,
             source_instance_id, updated_at)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT (option_id) DO UPDATE SET
            kind = excluded.kind,
            raw_key = excluded.raw_key,
            canonical_type = excluded.canonical_type,
            canonical_value_json = excluded.canonical_value_json,
            value_codec = excluded.value_codec,
            seeded = excluded.seeded,
            revision = excluded.revision,
            origin = excluded.origin,
            source_game_version = excluded.source_game_version,
            source_instance_id = excluded.source_instance_id,
            updated_at = excluded.updated_at
        ",
    )
    .bind(key)
    .bind(record.kind.as_str())
    .bind(&record.raw_key)
    .bind(&record.canonical_type)
    .bind(&record.canonical_value)
    .bind(&record.value_codec)
    .bind(record.seeded)
    .bind(record.revision)
    .bind(record.origin.as_str())
    .bind(&record.source_game_version)
    .bind(&record.source_instance_id)
    .bind(record.updated_at)
    .execute(&mut *conn)
    .await?;

    Ok(())
}

/// A value whose kind this build does not know says nothing about the setting,
/// so it is read as no record at all.
fn to_kind(kind: &str) -> Option<GameOptionValueKind> {
    GameOptionValueKind::parse(kind).or_else(|| {
        tracing::warn!(
            %kind,
            "Skipping a game setting whose kind this build does not know"
        );
        None
    })
}

/// The same holds for a value whose provenance cannot be read.
fn to_origin(origin: &str) -> Option<GameOptionValueOrigin> {
    GameOptionValueOrigin::parse(origin).or_else(|| {
        tracing::warn!(
            %origin,
            "Skipping a game setting whose origin this build does not know"
        );
        None
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::SqlitePool;

    async fn pool() -> SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();
        pool
    }

    fn vanilla() -> GameOptionValueRecord {
        GameOptionValueRecord {
            kind: GameOptionValueKind::Vanilla,
            raw_key: None,
            canonical_type: "bool".to_string(),
            canonical_value: Some("true".to_string()),
            value_codec: "catalog".to_string(),
            seeded: true,
            revision: 1,
            origin: GameOptionValueOrigin::Instance,
            source_game_version: Some("1.21".to_string()),
            source_instance_id: Some("instance".to_string()),
            updated_at: 7,
        }
    }

    #[tokio::test]
    async fn a_value_round_trips_and_clears() {
        let handle = Handle::new(pool().await);
        let option_id = "renderDistance".to_string();
        SyncedGameOptionValues::set(&handle, &option_id, Some(&vanilla()))
            .await
            .unwrap();

        assert_eq!(
            SyncedGameOptionValues::get(&handle, &option_id)
                .await
                .unwrap(),
            Some(vanilla())
        );

        SyncedGameOptionValues::set(&handle, &option_id, None)
            .await
            .unwrap();
        assert_eq!(
            SyncedGameOptionValues::get(&handle, &option_id)
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn a_value_this_build_cannot_read_is_no_record() {
        let pool = pool().await;
        let handle = Handle::new(pool.clone());
        sqlx::query("PRAGMA ignore_check_constraints = ON")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "
            INSERT INTO synced_game_option_values
                (option_id, kind, raw_key, canonical_type, value_codec,
                 canonical_value_json, seeded, revision, origin, updated_at)
            VALUES ('future', 'modded', NULL, 'bool', 'catalog', 'true',
                1, 1, 'app_editor', 0)
            ",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("PRAGMA ignore_check_constraints = OFF")
            .execute(&pool)
            .await
            .unwrap();

        assert_eq!(
            SyncedGameOptionValues::get(&handle, &"future".to_string())
                .await
                .unwrap(),
            None
        );
        assert!(
            SyncedGameOptionValues::get_all(&handle)
                .await
                .unwrap()
                .is_empty()
        );
    }
}
