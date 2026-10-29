//! The launcher's rebuildable records, in a database of their own.
//!
//! Nothing here is user data: every record can be recomputed, so a database
//! this build cannot open is disabled for the run instead of stopping the
//! launcher. The migration table SQLx keeps in the file is the only version
//! record; there is no `derived_meta` beside it, because a second source of
//! truth could not be checked against the first.
//!
//! A disabled database is left exactly as it is, since deleting it implicitly
//! would destroy what a newer launcher wrote before a downgrade. Rebuilding is
//! an explicit action: the entry point that rebuilds caches, or removing the
//! file by hand.
//!
//! While it is disabled its collections still answer: a read reports what an
//! empty database would, and a write is dropped after one warning. A missing
//! derived record must never become an error its caller has to handle.
//!
//! Disabling keeps an in-memory database in the file's place. Every table is
//! created there too, so the callers written before collections existed — the
//! ones that ask for a pool and run their own statements — can still be
//! called while the file is unavailable. Nothing they write survives the run,
//! which is the same answer their caller would get from a dropped write: for
//! derived records the durable answer is always that they are gone.

use sqlx::SqlitePool;
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions,
};
use std::ops::AsyncFnOnce;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::Handle;

/// The name of this database inside the cache directory. Clearing the cache
/// removes content, never the database file or its `-wal` / `-shm` sidecars.
pub(crate) const FILE_NAME: &str = "derived.db";

static MIGRATOR: sqlx::migrate::Migrator =
    sqlx::migrate!("./derived_migrations");

pub(crate) struct DerivedDb {
    path: PathBuf,
    pool: Option<SqlitePool>,
    disabled: bool,
}

impl DerivedDb {
    /// Opening failure costs the records, never the launch: a database holding
    /// an unknown version or an unreadable file stays disabled until it is
    /// rebuilt, and an in-memory database answers in its place.
    pub(crate) async fn open(path: PathBuf) -> Self {
        let mut disabled = false;
        let pool = match Self::connect(&path).await {
            Ok(pool) => Some(pool),
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    database = %path.display(),
                    "Derived database disabled; its records stay unwritten"
                );
                disabled = true;
                match Self::connect_in_memory().await {
                    Ok(pool) => Some(pool),
                    Err(error) => {
                        tracing::warn!(
                            error = %error,
                            "The derived database has no in-memory stand-in either"
                        );
                        None
                    }
                }
            }
        };
        let db = Self {
            path,
            pool,
            disabled,
        };
        if !db.disabled {
            tracing::info!(
                database = %db.path.display(),
                "Derived database ready"
            );
        }
        db
    }

    async fn connect(path: &Path) -> crate::Result<SqlitePool> {
        if let Some(parent) = path.parent() {
            crate::util::io::create_dir_all(parent).await?;
        }
        let pool = SqlitePoolOptions::new()
            .max_connections(3)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(path)
                    .create_if_missing(true)
                    .journal_mode(SqliteJournalMode::Wal)
                    .busy_timeout(Duration::from_secs(5))
                    .foreign_keys(true),
            )
            .await?;
        if let Err(error) = MIGRATOR.run(&pool).await {
            pool.close().await;
            return Err(error.into());
        }
        Ok(pool)
    }

    /// What a file this build cannot open is replaced with: a database that
    /// lives for the run and keeps nothing. One connection, kept for the whole
    /// run, because the database is lost with it.
    async fn connect_in_memory() -> crate::Result<SqlitePool> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .idle_timeout(None)
            .max_lifetime(None)
            .connect_with(SqliteConnectOptions::new().in_memory(true))
            .await?;
        if let Err(error) = MIGRATOR.run(&pool).await {
            pool.close().await;
            return Err(error.into());
        }
        Ok(pool)
    }

    /// Whether the records are going anywhere other than the file.
    pub(crate) fn is_disabled(&self) -> bool {
        self.disabled
    }

    /// What statements are run on, which is an in-memory database while the
    /// file is unavailable, or nothing if even that could not be opened.
    pub(crate) fn pool(&self) -> Option<&SqlitePool> {
        self.pool.as_ref()
    }

    /// What this database's records are read and written through, or nothing
    /// while it is disabled.
    pub(crate) fn handle(&self) -> Option<Handle> {
        self.pool().map(|pool| Handle::new(pool.clone()))
    }

    /// Reads through this database, and answers what an empty one would answer
    /// while it is disabled.
    pub(crate) async fn read<T, F>(&self, read: F) -> crate::Result<T>
    where
        T: Default,
        F: AsyncFnOnce(&Handle) -> crate::Result<T>,
    {
        if self.disabled {
            return Ok(T::default());
        }
        match self.handle() {
            Some(handle) => read(&handle).await,
            None => Ok(T::default()),
        }
    }

    /// Writes through this database, or drops the write while it is disabled:
    /// no caller can be failed by a record that cannot be kept.
    pub(crate) async fn write<F>(&self, write: F) -> crate::Result<()>
    where
        F: AsyncFnOnce(&Handle) -> crate::Result<()>,
    {
        if self.disabled {
            tracing::warn!(
                database = %self.path.display(),
                "Derived database disabled; the write is dropped"
            );
            return Ok(());
        }
        match self.handle() {
            Some(handle) => write(&handle).await,
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{DerivedDb, FILE_NAME};
    use crate::state::store::Collection as _;
    use crate::state::store::translation_cache::{
        TranslationCache, TranslationCacheRecord,
    };
    use crate::state::{CacheBehaviour, CacheValue, CachedEntry};
    use crate::util::fetch::FetchSemaphore;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::path::Path;
    use tokio::sync::Semaphore;

    /// A database that already ran a migration this build does not have.
    async fn with_newer_migration(path: &Path, version: i64) {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(path)
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE _sqlx_migrations (
                version BIGINT PRIMARY KEY,
                description TEXT NOT NULL,
                installed_on TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
                success BOOLEAN NOT NULL,
                checksum BLOB NOT NULL,
                execution_time BIGINT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO _sqlx_migrations
                (version, description, success, checksum, execution_time)
             VALUES (?, ?, 1, ?, 0)",
        )
        .bind(version)
        .bind("newer launcher")
        .bind(Vec::<u8>::new())
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;
    }

    #[tokio::test]
    async fn unknown_migration_version_disables_the_database() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(FILE_NAME);
        with_newer_migration(&path, 9_999_999_999_999_999).await;

        let db = DerivedDb::open(path.clone()).await;

        assert!(db.is_disabled(), "an unknown version must disable it");
        assert!(
            db.pool().is_some(),
            "a disabled database still answers with a pool"
        );
        assert!(path.exists(), "a disabled database must be kept as it is");
    }

    #[tokio::test]
    async fn unreadable_file_disables_the_database() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(FILE_NAME);
        tokio::fs::write(&path, b"not a database").await.unwrap();

        let db = DerivedDb::open(path.clone()).await;

        assert!(db.is_disabled(), "garbage bytes must disable it");
        assert!(
            db.pool().is_some(),
            "a disabled database still answers with a pool"
        );
        assert!(path.exists(), "a disabled database must be kept as it is");
    }

    #[tokio::test]
    async fn a_disabled_database_reads_as_empty_and_drops_writes() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(FILE_NAME);
        tokio::fs::write(&path, b"not a database").await.unwrap();
        let db = DerivedDb::open(path).await;
        let key = "key".to_string();
        let record = TranslationCacheRecord {
            translation: "text".to_string(),
            created_at: 1,
        };

        let one: Option<TranslationCacheRecord> = db
            .read(async |handle| TranslationCache::get(handle, &key).await)
            .await
            .unwrap();
        assert_eq!(one, None, "a disabled database holds no record");

        let all: Vec<(String, TranslationCacheRecord)> = db
            .read(async |handle| TranslationCache::get_all(handle).await)
            .await
            .unwrap();
        assert!(all.is_empty(), "a disabled database holds no collection");

        let written = db
            .write(async |handle| {
                TranslationCache::set(handle, &key, Some(&record)).await
            })
            .await;
        assert!(written.is_ok(), "a dropped write is not an error");
    }

    #[tokio::test]
    async fn fresh_database_opens() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(FILE_NAME);

        let db = DerivedDb::open(path.clone()).await;

        assert!(db.handle().is_some(), "an empty migration set is usable");
        assert!(!db.is_disabled(), "an opened file is not a stand-in");
        assert!(path.exists());
    }

    /// The cache rows a caller keeps in this database, read and written through
    /// the pool it asks for rather than through a collection.
    #[tokio::test]
    async fn the_cache_rows_round_trip_on_a_derived_handle() {
        let temp = tempfile::tempdir().unwrap();
        let db = DerivedDb::open(temp.path().join(FILE_NAME)).await;
        let pool = db.pool().expect("a fresh derived database opens");
        let semaphore = FetchSemaphore(Semaphore::new(1));
        let written =
            CacheValue::ReportTypes(vec!["tag".to_string()]).get_entry();

        CachedEntry::upsert_many(&[written], pool).await.unwrap();

        let read = CachedEntry::get_report_types(
            Some(CacheBehaviour::CacheOnly),
            pool,
            &semaphore,
        )
        .await
        .unwrap();
        assert_eq!(read, Some(vec!["tag".to_string()]));
    }

    /// What the callers that ask for a pool see while the file is unusable: a
    /// miss rather than an error, and a write that is not reported as failed.
    #[tokio::test]
    async fn a_disabled_database_answers_the_cache_with_a_miss() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(FILE_NAME);
        tokio::fs::write(&path, b"not a database").await.unwrap();
        let db = DerivedDb::open(path).await;
        let pool = db.pool().expect("a disabled database answers with a pool");
        let semaphore = FetchSemaphore(Semaphore::new(1));

        let read = CachedEntry::get_report_types(
            Some(CacheBehaviour::CacheOnly),
            pool,
            &semaphore,
        )
        .await;
        assert_eq!(read.unwrap(), None, "a disabled database holds no row");

        let write =
            CacheValue::ReportTypes(vec!["tag".to_string()]).get_entry();
        let written = CachedEntry::upsert_many(&[write], pool).await;
        assert!(written.is_ok(), "a discarded write is not an error");
    }
}
