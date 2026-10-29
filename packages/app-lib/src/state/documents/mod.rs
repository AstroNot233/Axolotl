//! The settings documents: one JSON file per domain, under
//! `<settings dir>/settings/`.
//!
//! A document is authoritative for its domain. A key the document leaves out
//! holds the type's default and never the column the domain used to be read
//! from; the row is only read while the domain has no document at all. That
//! keeps a build which predates the documents working, and it is the handover
//! below that ends it.
//!
//! The handover runs once: it turns the rows into documents and leaves
//! `.handed-over` behind, so an installation that took the row over never does
//! it again.

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;
use sqlx::SqlitePool;

use crate::state::State;

pub(crate) mod ai;
pub(crate) mod log_share;

const DIR_NAME: &str = "settings";
const HANDOVER_MARKER: &str = ".handed-over";

/// Serializes the writers, which each read a document before writing it back:
/// two settings changing at once would otherwise lose one of them.
static WRITES: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Where the documents of one launch live, and what the fallback reads.
pub(crate) struct Documents<'a> {
    root: PathBuf,
    pool: &'a SqlitePool,
}

impl<'a> Documents<'a> {
    pub(crate) fn new(state: &'a State) -> Self {
        Self {
            root: state.directories.settings_dir.join(DIR_NAME),
            pool: &state.pool,
        }
    }

    #[cfg(test)]
    pub(crate) fn at(root: PathBuf, pool: &'a SqlitePool) -> Self {
        Self { root, pool }
    }

    pub(crate) fn path(&self, name: &str) -> PathBuf {
        self.root.join(format!("{name}.json"))
    }

    pub(crate) fn pool(&self) -> &'a SqlitePool {
        self.pool
    }

    pub(crate) async fn stored<T>(&self, name: &str) -> Stored<T>
    where
        T: DeserializeOwned + Default,
    {
        let path = self.path(name);
        let contents = match tokio::fs::read(&path).await {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Stored::Missing;
            }
            Err(error) => {
                tracing::warn!(
                    path = %path.display(),
                    %error,
                    "Could not read a settings document"
                );
                return Stored::Unreadable;
            }
        };
        let Ok(stored) = serde_json::from_slice::<StoredDocument<T>>(&contents)
        else {
            tracing::warn!(
                path = %path.display(),
                "Ignoring a settings document that cannot be read"
            );
            return Stored::Unreadable;
        };
        Stored::Ready {
            schema_version: stored.schema_version,
            document: stored.data,
        }
    }

    /// Replaces a document with a temporary file and a rename, so a reader
    /// never sees half of one.
    pub(crate) async fn write<T>(
        &self,
        name: &str,
        schema_version: u32,
        document: &T,
    ) -> crate::Result<()>
    where
        T: Serialize,
    {
        let path = self.path(name);
        let contents = serde_json::to_vec_pretty(&NewDocument {
            schema_version,
            data: document,
        })?;
        if let Some(parent) = path.parent() {
            crate::util::io::create_dir_all(parent).await?;
        }
        crate::util::io::write(&path, contents).await?;
        Ok(())
    }
}

pub(crate) enum Stored<T> {
    /// No document, so the domain is still answered by its rows.
    Missing,
    /// A document that cannot be read: it is left alone and answered by the
    /// rows, so a damaged file does not silently empty the settings.
    Unreadable,
    Ready {
        schema_version: u32,
        document: T,
    },
}

#[derive(Serialize)]
struct NewDocument<'a, T: Serialize> {
    schema_version: u32,
    data: &'a T,
}

#[derive(serde::Deserialize)]
struct StoredDocument<T> {
    schema_version: u32,
    #[serde(default)]
    data: T,
}

/// Turns every domain that has no document yet over to one, and records that
/// the handover is finished once all of them are in place. Reports whether the
/// handover is on record; one that fails part way is attempted again at the
/// next start.
pub(crate) async fn hand_over(pool: &SqlitePool, settings_dir: &Path) -> bool {
    let documents = Documents {
        root: settings_dir.join(DIR_NAME),
        pool,
    };
    let marker = documents.root.join(HANDOVER_MARKER);
    if tokio::fs::try_exists(&marker).await.unwrap_or(false) {
        return true;
    }
    let ai_ready = ai::hand_over_if_missing(&documents).await;
    let log_share_ready = log_share::hand_over_if_missing(&documents).await;
    if !ai_ready || !log_share_ready {
        tracing::warn!(
            "The settings handover is incomplete; the next start retries it"
        );
        return false;
    }
    let written = async {
        if let Some(parent) = marker.parent() {
            crate::util::io::create_dir_all(parent).await?;
        }
        crate::util::io::write(&marker, env!("CARGO_PKG_VERSION")).await
    }
    .await;
    match written {
        Ok(()) => {
            tracing::info!("Handed the settings rows over to documents");
            true
        }
        Err(error) => {
            tracing::warn!(
                path = %marker.display(),
                %error,
                "Failed to record the settings handover"
            );
            false
        }
    }
}

pub(super) fn default_true() -> bool {
    true
}

pub(super) fn is_true(value: &bool) -> bool {
    *value
}

pub(super) fn is_false(value: &bool) -> bool {
    !*value
}

#[cfg(test)]
pub(crate) mod test_support {
    use sqlx::SqlitePool;
    use sqlx::sqlite::SqlitePoolOptions;

    /// One connection, so every statement sees the same in-memory database.
    pub(crate) async fn pool() -> SqlitePool {
        SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("in-memory SQLite pool")
    }

    /// The three AI tables, as the migrations leave them.
    pub(crate) async fn create_ai_tables(pool: &SqlitePool) {
        sqlx::query(
            "CREATE TABLE ai_settings (id INTEGER NOT NULL CHECK (id = 0), enabled INTEGER NOT NULL DEFAULT TRUE, legacy_openai_credential_cleanup INTEGER NOT NULL DEFAULT FALSE, PRIMARY KEY (id))",
        )
        .execute(pool)
        .await
        .expect("ai_settings table");
        sqlx::query("INSERT INTO ai_settings (id) VALUES (0)")
            .execute(pool)
            .await
            .expect("ai_settings row");
        sqlx::query(
            "CREATE TABLE ai_provider_configs (provider_id TEXT NOT NULL PRIMARY KEY, custom_name TEXT NULL, protocol TEXT NOT NULL DEFAULT 'openai', enabled INTEGER NOT NULL DEFAULT FALSE, endpoint TEXT NOT NULL DEFAULT '', settings TEXT NOT NULL DEFAULT '{}')",
        )
        .execute(pool)
        .await
        .expect("ai_provider_configs table");
        sqlx::query(
            "CREATE TABLE ai_provider_models (provider_id TEXT NOT NULL, model_id TEXT NOT NULL, display_name TEXT NOT NULL DEFAULT '', enabled INTEGER NOT NULL DEFAULT TRUE, source TEXT NOT NULL DEFAULT 'custom', PRIMARY KEY (provider_id, model_id))",
        )
        .execute(pool)
        .await
        .expect("ai_provider_models table");
    }

    /// The two log sharing tables, as the migrations leave them.
    pub(crate) async fn create_log_share_tables(pool: &SqlitePool) {
        sqlx::query(
            "CREATE TABLE log_share_settings (id INTEGER NOT NULL CHECK (id = 0), share_provider TEXT NOT NULL DEFAULT 'logshare', ai_source TEXT NOT NULL DEFAULT 'logshare', auto_upload INTEGER NOT NULL DEFAULT TRUE, multi_file INTEGER NOT NULL DEFAULT TRUE, metadata_enabled INTEGER NOT NULL DEFAULT TRUE, no_storage INTEGER NOT NULL DEFAULT FALSE, show_progress INTEGER NOT NULL DEFAULT TRUE, PRIMARY KEY (id))",
        )
        .execute(pool)
        .await
        .expect("log_share_settings table");
        sqlx::query("INSERT INTO log_share_settings (id) VALUES (0)")
            .execute(pool)
            .await
            .expect("log_share_settings row");
        sqlx::query(
            "CREATE TABLE crash_analysis_ai_settings (id INTEGER NOT NULL CHECK (id = 0), enabled INTEGER NOT NULL DEFAULT FALSE, provider_id TEXT NOT NULL DEFAULT '', model_id TEXT NOT NULL DEFAULT '', ai_source TEXT NOT NULL DEFAULT 'logshare', PRIMARY KEY (id))",
        )
        .execute(pool)
        .await
        .expect("crash_analysis_ai_settings table");
        sqlx::query("INSERT INTO crash_analysis_ai_settings (id) VALUES (0)")
            .execute(pool)
            .await
            .expect("crash_analysis_ai_settings row");
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{
        create_ai_tables, create_log_share_tables, pool,
    };
    use super::{Documents, ai, hand_over, log_share};

    async fn installed_pool() -> sqlx::SqlitePool {
        let pool = pool().await;
        create_ai_tables(&pool).await;
        create_log_share_tables(&pool).await;
        pool
    }

    #[tokio::test]
    async fn a_handover_stops_the_rows_from_being_read() {
        let pool = installed_pool().await;
        sqlx::query("UPDATE ai_settings SET enabled = FALSE")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("UPDATE log_share_settings SET share_provider = 'mclogs'")
            .execute(&pool)
            .await
            .unwrap();

        let settings_dir = tempfile::tempdir().unwrap();
        assert!(hand_over(&pool, settings_dir.path()).await);
        assert!(
            settings_dir
                .path()
                .join("settings")
                .join(".handed-over")
                .is_file()
        );

        let root = settings_dir.path().join("settings");
        let documents = Documents::at(root, &pool);
        assert!(!ai::read(&documents).await.unwrap().enabled);
        assert_eq!(
            log_share::read(&documents).await.unwrap().share_provider,
            "mclogs"
        );

        sqlx::query("UPDATE ai_settings SET enabled = TRUE")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "UPDATE log_share_settings SET share_provider = 'logshare'",
        )
        .execute(&pool)
        .await
        .unwrap();

        assert!(!ai::read(&documents).await.unwrap().enabled);
        assert_eq!(
            log_share::read(&documents).await.unwrap().share_provider,
            "mclogs"
        );
    }

    #[tokio::test]
    async fn a_handover_leaves_a_document_that_exists_alone() {
        let pool = installed_pool().await;
        sqlx::query("UPDATE ai_settings SET enabled = FALSE")
            .execute(&pool)
            .await
            .unwrap();

        let settings_dir = tempfile::tempdir().unwrap();
        let root = settings_dir.path().join("settings");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("ai.json"),
            r#"{"schema_version":1,"data":{}}"#,
        )
        .unwrap();

        assert!(hand_over(&pool, settings_dir.path()).await);

        let documents = Documents::at(root, &pool);
        assert!(
            ai::read(&documents).await.unwrap().enabled,
            "an existing document is authoritative, so the handover must not rewrite it"
        );
    }
}
