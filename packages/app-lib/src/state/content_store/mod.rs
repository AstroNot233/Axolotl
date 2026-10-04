//! Cross-instance verified content objects.

use crate::state::DirectoryInfo;
use crate::util::io;
use dashmap::DashMap;
use sqlx::SqlitePool;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use tokio::sync::Mutex;

static ACQUIRE_LOCKS: LazyLock<DashMap<String, Arc<Mutex<()>>>> =
    LazyLock::new(DashMap::new);

pub(crate) fn object_path(
    directories: &DirectoryInfo,
    sha512: &str,
) -> PathBuf {
    directories.content_store_dir().join(sha512)
}

pub(crate) async fn acquire_lock(sha512: &str) -> Arc<Mutex<()>> {
    ACQUIRE_LOCKS
        .entry(sha512.to_ascii_lowercase())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

pub(crate) async fn publish_verified(
    source: &Path,
    directories: &DirectoryInfo,
    sha512: &str,
) -> crate::Result<PathBuf> {
    let lock = acquire_lock(sha512).await;
    let _guard = lock.lock().await;
    let destination = object_path(directories, sha512);
    if tokio::fs::try_exists(&destination).await.unwrap_or(false) {
        return Ok(destination);
    }
    if let Some(parent) = destination.parent() {
        io::create_dir_all(parent).await?;
    }
    let mut temporary = destination.as_os_str().to_os_string();
    temporary.push(".installing");
    let temporary = PathBuf::from(temporary);
    if tokio::fs::try_exists(&temporary).await.unwrap_or(false) {
        io::remove_file(&temporary).await?;
    }
    io::copy(source, &temporary).await?;
    if let Err(error) = tokio::fs::rename(&temporary, &destination).await {
        let _ = io::remove_file(&temporary).await;
        return Err(error.into());
    }
    Ok(destination)
}

pub(crate) async fn record_published(
    pool: &SqlitePool,
    digest: &str,
    size: u64,
) -> crate::Result<()> {
    let now = chrono::Utc::now().timestamp();
    sqlx::query(
        "INSERT INTO store_blobs (digest, size, state, created_at, last_used_at) VALUES (?, ?, 'ready', ?, ?) ON CONFLICT(digest) DO UPDATE SET size = excluded.size, state = 'ready', last_used_at = excluded.last_used_at",
    )
    .bind(digest)
    .bind(size as i64)
    .bind(now)
    .bind(now)
    .execute(pool)
    .await?;
    Ok(())
}
