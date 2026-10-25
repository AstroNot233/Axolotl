//! What the instance file steps promise: which files survive a rollback, and
//! which stored metadata answers for the bytes a file carries.

use super::*;

async fn pool_with_files(
    rows: &[(&str, &str, &str, i64, Option<&str>)],
) -> sqlx::SqlitePool {
    let pool =
        crate::state::instances::adapters::sqlite::test_support::test_pool()
            .await;
    sqlx::raw_sql(
        "
        CREATE TABLE instance_files (
            id TEXT PRIMARY KEY,
            instance_id TEXT NOT NULL,
            relative_path TEXT NOT NULL,
            file_name TEXT NOT NULL,
            enabled INTEGER NOT NULL,
            sha1 TEXT NOT NULL,
            size INTEGER NOT NULL,
            missing INTEGER NOT NULL DEFAULT 0,
            added_at INTEGER NOT NULL,
            modified_at INTEGER NOT NULL,
            local_mod_data TEXT NULL,
            icon_path TEXT NULL
        );
        CREATE TABLE instance_content_entries (
            id TEXT PRIMARY KEY,
            file_id TEXT NULL
        );
        ",
    )
    .execute(&pool)
    .await
    .expect("file tables");

    for (id, relative_path, sha1, size, local_mod_data) in rows {
        sqlx::query(
            "INSERT INTO instance_files
                (id, instance_id, relative_path, file_name, enabled, sha1, size,
                 missing, added_at, modified_at, local_mod_data)
             VALUES (?, 'instance', ?, 'file.jar', 1, ?, ?, 0, 1, 1, ?)",
        )
        .bind(*id)
        .bind(*relative_path)
        .bind(*sha1)
        .bind(*size)
        .bind(*local_mod_data)
        .execute(&pool)
        .await
        .unwrap();
    }

    pool
}

async fn remaining_files(pool: &sqlx::SqlitePool) -> Vec<String> {
    sqlx::query_scalar("SELECT id FROM instance_files ORDER BY id")
        .fetch_all(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn a_file_an_entry_names_survives_the_rollback_and_an_entryless_one_does_not()
 {
    let pool = pool_with_files(&[
        ("named", "mods/named.jar", "sha1:a", 10, None),
        ("entryless", "mods/entryless.jar", "sha1:b", 20, None),
    ])
    .await;
    sqlx::query(
        "INSERT INTO instance_content_entries (id, file_id) VALUES ('entry', 'named')",
    )
    .execute(&pool)
    .await
    .unwrap();

    let mut tx = pool.begin().await.unwrap();
    delete_unreferenced_file_by_id_in_transaction("instance", "named", &mut tx)
        .await
        .unwrap();
    delete_unreferenced_file_by_id_in_transaction(
        "instance",
        "entryless",
        &mut tx,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(remaining_files(&pool).await, ["named"]);
}

#[tokio::test]
async fn the_metadata_of_files_with_the_same_bytes_reads_back() {
    let pool = pool_with_files(&[
        (
            "same",
            "mods/same.jar",
            "sha1:a",
            10,
            Some("{\"name\":\"same\"}"),
        ),
        (
            "other",
            "mods/other.jar",
            "sha1:a",
            20,
            Some("{\"name\":\"other\"}"),
        ),
        (
            "gone",
            "mods/gone.jar",
            "sha1:a",
            10,
            Some("{\"name\":\"gone\"}"),
        ),
    ])
    .await;
    sqlx::query("UPDATE instance_files SET missing = 1 WHERE id = 'gone'")
        .execute(&pool)
        .await
        .unwrap();

    let cached = get_local_mod_data_for_artifact("sha1:a", 10, &pool)
        .await
        .unwrap();

    assert_eq!(cached, ["{\"name\":\"same\"}"]);
}

#[tokio::test]
async fn the_metadata_recorded_for_a_file_reads_back() {
    let pool =
        pool_with_files(&[("file", "mods/file.jar", "sha1:a", 10, None)]).await;

    let mut tx = pool.begin().await.unwrap();
    set_local_mod_data_in_transaction(
        "file",
        Some("{\"name\":\"recorded\"}"),
        &mut tx,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let cached = get_local_mod_data_for_artifact("sha1:a", 10, &pool)
        .await
        .unwrap();

    assert_eq!(cached, ["{\"name\":\"recorded\"}"]);
}

async fn cleanup_pool() -> sqlx::SqlitePool {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("in-memory SQLite pool");
    sqlx::raw_sql(
        "CREATE TABLE instance_files (
                 id TEXT PRIMARY KEY,
                 instance_id TEXT NOT NULL,
                 relative_path TEXT NOT NULL
             );
             CREATE TABLE instance_content_entries (
                 id TEXT PRIMARY KEY,
                 instance_id TEXT NOT NULL,
                 file_id TEXT
             );",
    )
    .execute(&pool)
    .await
    .expect("instance tables");
    pool
}

async fn insert_file(
    pool: &sqlx::SqlitePool,
    id: &str,
    instance_id: &str,
    relative_path: &str,
) {
    sqlx::query(
            "INSERT INTO instance_files (id, instance_id, relative_path) VALUES (?, ?, ?)",
        )
        .bind(id)
        .bind(instance_id)
        .bind(relative_path)
        .execute(pool)
        .await
        .expect("insert instance file");
}

async fn insert_entry(
    pool: &sqlx::SqlitePool,
    id: &str,
    instance_id: &str,
    file_id: &str,
) {
    sqlx::query(
            "INSERT INTO instance_content_entries (id, instance_id, file_id) VALUES (?, ?, ?)",
        )
        .bind(id)
        .bind(instance_id)
        .bind(file_id)
        .execute(pool)
        .await
        .expect("insert content entry");
}

#[tokio::test]
async fn cleanup_removes_only_rows_outside_the_scan_scope() {
    let pool = cleanup_pool().await;
    // Content the scan reports, plus a scannable path whose file was genuinely
    // deleted: both must survive the cleanup.
    insert_file(&pool, "tracked", "instance", "mods/example.jar").await;
    insert_entry(&pool, "tracked-entry", "instance", "tracked").await;
    insert_file(
        &pool,
        "deleted",
        "instance",
        "schematics/old/house.litematic",
    )
    .await;
    insert_entry(&pool, "deleted-entry", "instance", "deleted").await;
    // Legacy rows the scan can never report: an Iris shader sidecar and an
    // update backup.
    insert_file(
        &pool,
        "sidecar",
        "instance",
        "shaderpacks/BSL_v10.1.3.zip.txt",
    )
    .await;
    insert_entry(&pool, "sidecar-entry", "instance", "sidecar").await;
    insert_file(
        &pool,
        "backup",
        "instance",
        "mods/Mod-1.0.jar_Mod-0.9.jar.old",
    )
    .await;
    // Another instance keeps its own rows.
    insert_file(
        &pool,
        "other",
        "other-instance",
        "shaderpacks/BSL_v10.1.3.zip.txt",
    )
    .await;

    let mut tx = pool.begin().await.expect("transaction");
    super::remove_untracked_instance_files("instance", &mut tx)
        .await
        .expect("cleanup");
    tx.commit().await.expect("commit");

    let files: Vec<String> =
        sqlx::query_scalar("SELECT id FROM instance_files ORDER BY id")
            .fetch_all(&pool)
            .await
            .expect("read files");
    assert_eq!(files, vec!["deleted", "other", "tracked"]);

    let entries: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM instance_content_entries ORDER BY id",
    )
    .fetch_all(&pool)
    .await
    .expect("read entries");
    assert_eq!(entries, vec!["deleted-entry", "tracked-entry"]);
}
