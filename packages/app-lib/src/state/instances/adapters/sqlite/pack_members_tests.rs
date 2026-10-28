use super::*;
use crate::state::{ContentOwnershipKind, ContentProvider, ProjectType};
use sqlx::SqlitePool;
use sqlx::sqlite::SqlitePoolOptions;
async fn test_pool() -> SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("in-memory SQLite pool");
    sqlx::query("CREATE TABLE instances (id TEXT PRIMARY KEY)")
        .execute(&pool)
        .await
        .expect("instances table");
    sqlx::query("CREATE TABLE instance_content_sets (id TEXT PRIMARY KEY)")
        .execute(&pool)
        .await
        .expect("instance_content_sets table");
    pool
}

#[tokio::test]
async fn materialized_curseforge_downloads_require_current_exact_content() {
    let pool = test_pool().await;
    sqlx::query(
        "ALTER TABLE instances ADD COLUMN applied_content_set_id TEXT NULL",
    )
    .execute(&pool)
    .await
    .expect("applied content set column");
    sqlx::raw_sql(
        "
        CREATE TABLE instance_files (
            id TEXT PRIMARY KEY,
            instance_id TEXT NOT NULL,
            missing INTEGER NOT NULL
        );
        CREATE TABLE instance_content_entries (
            id TEXT PRIMARY KEY,
            content_set_id TEXT NOT NULL,
            file_id TEXT NULL
        );
        CREATE TABLE instance_content_provider_refs (
            content_entry_id TEXT NOT NULL,
            provider TEXT NOT NULL,
            provider_project_id TEXT NOT NULL,
            provider_release_id TEXT NULL
        );
        CREATE TABLE instance_pack_members (
            id TEXT PRIMARY KEY,
            content_set_id TEXT NOT NULL,
            content_entry_id TEXT NULL,
            provider TEXT NULL,
            provider_project_id TEXT NULL,
            provider_release_id TEXT NULL,
            materialization_state TEXT NOT NULL
        );
        ",
    )
    .execute(&pool)
    .await
    .expect("content tables");
    sqlx::query(
        "INSERT INTO instance_content_sets (id) VALUES ('target-set'), ('other-set')",
    )
    .execute(&pool)
    .await
    .expect("content sets");
    sqlx::query(
        "INSERT INTO instances (id, applied_content_set_id)
         VALUES ('target', 'target-set'), ('other', 'other-set')",
    )
    .execute(&pool)
    .await
    .expect("instances");
    sqlx::query(
        "INSERT INTO instance_files (id, instance_id, missing) VALUES
            ('file-present', 'target', 0),
            ('file-member-missing', 'target', 0),
            ('file-untracked', 'target', 0),
            ('file-missing', 'target', 1),
            ('file-other', 'other', 0)",
    )
    .execute(&pool)
    .await
    .expect("files");
    sqlx::query(
        "INSERT INTO instance_content_entries (id, content_set_id, file_id) VALUES
            ('entry-present', 'target-set', 'file-present'),
            ('entry-member-missing', 'target-set', 'file-member-missing'),
            ('entry-untracked', 'target-set', 'file-untracked'),
            ('entry-missing', 'target-set', 'file-missing'),
            ('entry-other', 'other-set', 'file-other')",
    )
    .execute(&pool)
    .await
    .expect("entries");
    sqlx::query(
        "INSERT INTO instance_content_provider_refs (
            content_entry_id, provider, provider_project_id,
            provider_release_id
         ) VALUES
            ('entry-present', 'curseforge', '1', '10'),
            ('entry-member-missing', 'curseforge', '1', '11'),
            ('entry-untracked', 'curseforge', '2', '20'),
            ('entry-missing', 'curseforge', '3', '30'),
            ('entry-other', 'curseforge', '9', '90')",
    )
    .execute(&pool)
    .await
    .expect("provider refs");
    sqlx::query(
        "INSERT INTO instance_pack_members (
            id, content_set_id, content_entry_id, provider,
            provider_project_id, provider_release_id,
            materialization_state
         ) VALUES
            ('member-present', 'target-set', 'entry-present',
                'curseforge', '1', '10', 'present'),
            ('member-missing', 'target-set', 'entry-member-missing',
                'curseforge', '1', '11', 'missing'),
            ('member-other', 'other-set', 'entry-other',
                'curseforge', '9', '90', 'present')",
    )
    .execute(&pool)
    .await
    .expect("pack members");

    let materialized = get_materialized_curseforge_downloads("target", &pool)
        .await
        .expect("materialized CurseForge downloads")
        .into_iter()
        .collect::<std::collections::HashSet<_>>();

    assert_eq!(
        materialized,
        std::collections::HashSet::from([
            ("1".to_string(), "10".to_string()),
            ("2".to_string(), "20".to_string()),
        ])
    );
}

async fn mutation_target_pool() -> SqlitePool {
    let pool = test_pool().await;
    sqlx::query(
        "ALTER TABLE instances ADD COLUMN applied_content_set_id TEXT NULL",
    )
    .execute(&pool)
    .await
    .expect("applied content set column");
    sqlx::raw_sql(
        "
        CREATE TABLE instance_files (
            id TEXT PRIMARY KEY,
            instance_id TEXT NOT NULL,
            relative_path TEXT NOT NULL
        );
        CREATE TABLE instance_content_entries (
            id TEXT PRIMARY KEY,
            content_set_id TEXT NOT NULL,
            file_id TEXT NULL,
            ownership_kind TEXT NOT NULL,
            project_type TEXT NOT NULL
        );
        CREATE TABLE instance_pack_members (
            id TEXT PRIMARY KEY,
            content_set_id TEXT NOT NULL,
            content_entry_id TEXT NULL,
            project_type TEXT NOT NULL,
            provider TEXT NULL,
            provider_project_id TEXT NULL,
            provider_release_id TEXT NULL
        );
        CREATE TABLE instance_content_provider_refs (
            content_entry_id TEXT NOT NULL,
            provider TEXT NOT NULL,
            provider_project_id TEXT NOT NULL,
            provider_release_id TEXT NULL,
            is_origin INTEGER NOT NULL
        );
        ",
    )
    .execute(&pool)
    .await
    .expect("content tables");
    sqlx::query("INSERT INTO instance_content_sets (id) VALUES ('set')")
        .execute(&pool)
        .await
        .expect("insert content set");
    sqlx::query(
        "INSERT INTO instances (id, applied_content_set_id) VALUES ('instance', 'set')",
    )
    .execute(&pool)
    .await
    .expect("insert instance");
    sqlx::query(
        "INSERT INTO instance_files (id, instance_id, relative_path)
         VALUES ('file', 'instance', 'mods/example.jar')",
    )
    .execute(&pool)
    .await
    .expect("insert untracked file");

    pool
}

#[tokio::test]
async fn mutation_target_supports_untracked_instance_files() {
    let pool = mutation_target_pool().await;

    let target = get_content_mutation_target("instance", "file", &pool)
        .await
        .expect("resolve mutation target")
        .expect("mutation target exists");

    assert_eq!(target.entry_id, None);
    assert_eq!(target.member_id, None);
    assert_eq!(target.relative_path.as_deref(), Some("mods/example.jar"));
    assert_eq!(target.ownership_kind, ContentOwnershipKind::UserAdded);
    assert_eq!(target.project_type, ProjectType::Mod);
}

#[tokio::test]
async fn mutation_target_resolves_identified_mod_after_pack_disconnect() {
    let pool = mutation_target_pool().await;
    sqlx::raw_sql(
        "INSERT INTO instance_content_entries VALUES
            ('entry', 'set', 'file', 'user_added', 'mod');
         INSERT INTO instance_content_provider_refs VALUES
            ('entry', 'modrinth', 'AANobbMI', '7pwil2dy', 0);",
    )
    .execute(&pool)
    .await
    .unwrap();

    let target = get_content_mutation_target("instance", "entry", &pool)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(target.provider, Some(ContentProvider::Modrinth));
    assert_eq!(target.provider_project_id.as_deref(), Some("AANobbMI"));
    assert_eq!(target.provider_release_id.as_deref(), Some("7pwil2dy"));
    assert_eq!(target.relative_path.as_deref(), Some("mods/example.jar"));
    assert_eq!(target.ownership_kind, ContentOwnershipKind::UserAdded);

    // A different provider ref must prevent an implicit provider switch.
    sqlx::query(
        "INSERT INTO instance_content_provider_refs VALUES
        ('entry', 'curseforge', '123', '456', 0)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let target = get_content_mutation_target("instance", "entry", &pool)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(target.provider, None);

    sqlx::query(
        "UPDATE instance_content_provider_refs SET is_origin = 1
        WHERE provider = 'curseforge'",
    )
    .execute(&pool)
    .await
    .unwrap();
    let target = get_content_mutation_target("instance", "entry", &pool)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(target.provider, Some(ContentProvider::CurseForge));
    assert_eq!(target.provider_project_id.as_deref(), Some("123"));
    assert_eq!(target.provider_release_id.as_deref(), Some("456"));
}
