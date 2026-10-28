#![allow(dead_code)]

use super::values::{project_type_from_str, timestamp, unsigned};
use crate::state::instances::{
    ContentOwnershipKind, ManualDownloadOperationKind, ManualDownloadState,
    PackMember, PackMemberMaterializationState, PackMemberOverrideKind,
    PendingManualDownload,
};
use crate::state::{ContentProvider, ProjectType};
use chrono::Utc;
use sqlx::{Row, Sqlite, SqlitePool, Transaction};

// Call sites still name `content_rows`; these paths outlive the split.
pub(crate) use super::content_entries::{
    ContentEntryRow, ContentUpdateCheckRow, UpsertContentEntry,
    get_content_dependency_edges, get_content_entries,
    get_content_entry_by_file, get_content_entry_by_id,
    get_content_entry_by_provider_project, get_content_entry_by_provider_ref,
    get_content_entry_by_relative_path, get_content_origin_provider,
    get_content_provider_refs, get_content_provider_refs_with_origin,
    get_content_update_check, get_dependency_backfilled_entry_ids,
    get_orphaned_auto_dependencies,
    invalidate_exact_provider_refs_for_file_in_transaction,
    remove_content_entries_for_file,
    replace_content_dependency_edges_for_parent,
    restore_content_entry_in_transaction,
    restore_content_update_check_in_transaction,
    set_content_entry_auto_dependency,
    set_content_entry_auto_dependency_in_transaction,
    set_content_entry_dependency_backfilled_in_transaction,
    set_content_entry_enabled_for_file,
    upsert_content_dependency_edge_in_transaction,
    upsert_content_entry_from_parts,
    upsert_content_entry_from_parts_in_transaction,
    upsert_content_provider_ref, upsert_content_provider_ref_in_transaction,
    upsert_content_update_check,
};
pub(crate) use super::content_sets::{
    ContentSetRemoteRefRow, ContentSetRow, ContentSetSyncStateRow,
    bump_content_set_revision_in_transaction, get_applied_content_set,
    get_content_set, get_content_set_remote_refs, get_content_set_sync_state,
    get_content_sets_for_instance, insert_content_set, update_content_set,
};
pub(crate) use super::files::{
    InstanceFileRow, UpsertInstanceFile, adopt_untracked_file_in_transaction,
    get_instance_file_by_relative_path, get_instance_files,
    mark_instance_files_missing, move_instance_file_in_transaction,
    remove_instance_file_by_relative_path, rename_instance_file,
    rename_instance_file_in_transaction, upsert_instance_file,
    upsert_instance_file_from_parts,
    upsert_instance_file_from_parts_in_transaction,
};
pub(crate) use super::write_guards::{
    ensure_content_write_parents, ensure_instance_exists,
};

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct PackMemberRow {
    id: String,
    content_set_id: String,
    content_entry_id: Option<String>,
    member_key: String,
    project_type: String,
    expected_relative_path: String,
    provider: Option<String>,
    provider_project_id: Option<String>,
    provider_release_id: Option<String>,
    required: i64,
    expected_sha1: Option<String>,
    expected_size: Option<i64>,
    expected_fingerprint: Option<i64>,
    materialization_state: String,
    override_kind: String,
    reconciled: i64,
    created_at: i64,
    modified_at: i64,
}

impl TryFrom<PackMemberRow> for PackMember {
    type Error = crate::Error;

    fn try_from(row: PackMemberRow) -> crate::Result<Self> {
        Ok(Self {
            id: row.id,
            content_set_id: row.content_set_id,
            content_entry_id: row.content_entry_id,
            member_key: row.member_key,
            project_type: project_type_from_str(&row.project_type)?,
            expected_relative_path: row.expected_relative_path,
            provider: row
                .provider
                .as_deref()
                .map(ContentProvider::from_str)
                .transpose()?,
            provider_project_id: row.provider_project_id,
            provider_release_id: row.provider_release_id,
            required: row.required != 0,
            expected_sha1: row.expected_sha1,
            expected_size: row
                .expected_size
                .map(|value| unsigned(value, "expected_size"))
                .transpose()?,
            expected_fingerprint: row
                .expected_fingerprint
                .map(|value| unsigned(value, "expected_fingerprint"))
                .transpose()?,
            materialization_state: PackMemberMaterializationState::from_str(
                &row.materialization_state,
            )?,
            override_kind: PackMemberOverrideKind::from_str(
                &row.override_kind,
            )?,
            reconciled: row.reconciled != 0,
            created_at: timestamp(row.created_at),
            modified_at: timestamp(row.modified_at),
        })
    }
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct PendingManualDownloadRow {
    id: String,
    instance_id: String,
    pack_member_id: Option<String>,
    content_entry_id: Option<String>,
    operation_kind: String,
    operation_target_id: Option<String>,
    project_type: String,
    provider: String,
    provider_project_id: String,
    provider_release_id: String,
    file_name: String,
    website_url: Option<String>,
    target_relative_path: String,
    expected_sha1: Option<String>,
    expected_size: Option<i64>,
    expected_fingerprint: Option<i64>,
    state: String,
    context: String,
    created_at: i64,
    modified_at: i64,
}

impl TryFrom<PendingManualDownloadRow> for PendingManualDownload {
    type Error = crate::Error;

    fn try_from(row: PendingManualDownloadRow) -> crate::Result<Self> {
        Ok(Self {
            id: row.id,
            instance_id: row.instance_id,
            pack_member_id: row.pack_member_id,
            content_entry_id: row.content_entry_id,
            operation_kind: ManualDownloadOperationKind::from_str(
                &row.operation_kind,
            )?,
            operation_target_id: row.operation_target_id,
            project_type: project_type_from_str(&row.project_type)?,
            provider: ContentProvider::from_str(&row.provider)?,
            provider_project_id: row.provider_project_id,
            provider_release_id: row.provider_release_id,
            file_name: row.file_name,
            website_url: row.website_url,
            target_relative_path: row.target_relative_path,
            expected_sha1: row.expected_sha1,
            expected_size: row
                .expected_size
                .map(|value| unsigned(value, "expected_size"))
                .transpose()?,
            expected_fingerprint: row
                .expected_fingerprint
                .map(|value| unsigned(value, "expected_fingerprint"))
                .transpose()?,
            state: ManualDownloadState::from_str(&row.state)?,
            context: serde_json::from_str(&row.context).map_err(|error| {
                crate::ErrorKind::InputError(format!(
                    "Invalid pending manual download context: {error}"
                ))
            })?,
            created_at: timestamp(row.created_at),
            modified_at: timestamp(row.modified_at),
        })
    }
}

pub(crate) async fn get_pack_members(
    content_set_id: &str,
    pool: &SqlitePool,
) -> crate::Result<Vec<PackMember>> {
    let rows = sqlx::query_as::<_, PackMemberRow>(
        "SELECT * FROM instance_pack_members
         WHERE content_set_id = ?
         ORDER BY created_at ASC, id ASC",
    )
    .bind(content_set_id)
    .fetch_all(pool)
    .await?;

    rows.into_iter().map(TryInto::try_into).collect()
}

pub(crate) async fn get_materialized_curseforge_downloads(
    instance_id: &str,
    pool: &SqlitePool,
) -> crate::Result<Vec<(String, String)>> {
    let rows = sqlx::query(
        "SELECT DISTINCT ref.provider_project_id, ref.provider_release_id
         FROM instances instance
         INNER JOIN instance_content_entries entry
            ON entry.content_set_id = instance.applied_content_set_id
         INNER JOIN instance_files file
            ON file.id = entry.file_id AND file.instance_id = instance.id
         INNER JOIN instance_content_provider_refs ref
            ON ref.content_entry_id = entry.id
            AND ref.provider = 'curseforge'
         LEFT JOIN instance_pack_members member
            ON member.content_set_id = entry.content_set_id
            AND member.content_entry_id = entry.id
         WHERE instance.id = ?
            AND file.missing = 0
            AND ref.provider_release_id IS NOT NULL
            AND (
                member.id IS NULL
                OR (
                    member.provider = 'curseforge'
                    AND member.provider_project_id = ref.provider_project_id
                    AND member.provider_release_id = ref.provider_release_id
                    AND member.materialization_state = 'present'
                )
            )",
    )
    .bind(instance_id)
    .fetch_all(pool)
    .await?;

    rows.into_iter()
        .map(|row| {
            Ok((
                row.try_get::<String, _>("provider_project_id")?,
                row.try_get::<String, _>("provider_release_id")?,
            ))
        })
        .collect()
}

#[derive(Clone, Debug)]
pub(crate) struct ContentMutationTarget {
    pub entry_id: Option<String>,
    pub member_id: Option<String>,
    pub relative_path: Option<String>,
    pub ownership_kind: ContentOwnershipKind,
    pub project_type: ProjectType,
    pub provider: Option<ContentProvider>,
    pub provider_project_id: Option<String>,
    pub provider_release_id: Option<String>,
}

pub(crate) async fn get_content_mutation_target(
    instance_id: &str,
    target_id: &str,
    pool: &SqlitePool,
) -> crate::Result<Option<ContentMutationTarget>> {
    // Hash identification creates non-origin refs, including for unpacked mods
    // after disconnecting a pack. Match the update check's Modrinth fallback:
    // use those refs only when no origin/member or other provider takes priority.
    let row = sqlx::query(
        "SELECT entry.id AS entry_id,
            member.id AS member_id,
            file.relative_path,
            COALESCE(
                NULLIF(entry.ownership_kind, ''),
                CASE
                    WHEN member.id IS NULL THEN 'user_added'
                    ELSE 'pack_managed'
                END
            ) AS ownership_kind,
            COALESCE(
                NULLIF(entry.project_type, ''),
                NULLIF(member.project_type, '')
            ) AS project_type,
            COALESCE(origin.provider, member.provider, identified.provider) AS provider,
            COALESCE(origin.provider_project_id, member.provider_project_id, identified.provider_project_id)
                AS provider_project_id,
            COALESCE(origin.provider_release_id, member.provider_release_id, identified.provider_release_id)
                AS provider_release_id
         FROM instance_content_sets content_set
         INNER JOIN instances instance
            ON instance.applied_content_set_id = content_set.id
         LEFT JOIN instance_pack_members member
            ON member.content_set_id = content_set.id AND member.id = ?
         LEFT JOIN instance_content_entries entry
            ON entry.content_set_id = content_set.id
            AND (entry.id = ? OR entry.id = member.content_entry_id)
         LEFT JOIN instance_files file
            ON file.instance_id = instance.id
            AND (file.id = ? OR file.id = entry.file_id)
         LEFT JOIN instance_content_provider_refs origin
            ON origin.content_entry_id = entry.id AND origin.is_origin = 1
         LEFT JOIN instance_content_provider_refs identified
            ON origin.provider IS NULL AND member.provider IS NULL
            AND identified.rowid = (
                SELECT ref.rowid FROM instance_content_provider_refs ref
                WHERE ref.content_entry_id = entry.id AND ref.provider = 'modrinth'
                    AND NOT EXISTS (
                        SELECT 1 FROM instance_content_provider_refs other
                        WHERE other.content_entry_id = entry.id AND other.provider != 'modrinth'
                    )
                ORDER BY ref.provider_project_id, ref.provider_release_id IS NULL,
                    ref.provider_release_id
                LIMIT 1
            )
         WHERE instance.id = ?
            AND (entry.id = ? OR member.id = ? OR file.id = ?)
         LIMIT 1",
    )
    .bind(target_id)
    .bind(target_id)
    .bind(target_id)
    .bind(instance_id)
    .bind(target_id)
    .bind(target_id)
    .bind(target_id)
    .fetch_optional(pool)
    .await?;

    row.map(|row| {
        let relative_path = row.try_get::<Option<String>, _>("relative_path")?;
        let project_type = row
            .try_get::<Option<String>, _>("project_type")?
            .as_deref()
            .map(project_type_from_str)
            .transpose()?
            .or_else(|| {
                relative_path.as_deref().and_then(
                    crate::state::instances::adapters::filesystem::project_type_from_relative_path,
                )
            })
            .ok_or_else(|| {
                crate::ErrorKind::InputError(
                    "Unable to determine the selected content project type"
                        .to_string(),
                )
            })?;

        Ok(ContentMutationTarget {
            entry_id: row.try_get("entry_id")?,
            member_id: row.try_get("member_id")?,
            relative_path,
            ownership_kind: ContentOwnershipKind::from_str(
                &row.try_get::<String, _>("ownership_kind")?,
            )?,
            project_type,
            provider: row
                .try_get::<Option<String>, _>("provider")?
                .as_deref()
                .map(ContentProvider::from_str)
                .transpose()?,
            provider_project_id: row.try_get("provider_project_id")?,
            provider_release_id: row.try_get("provider_release_id")?,
        })
    })
    .transpose()
}

pub(crate) async fn set_pack_member_override_in_transaction(
    member_id: &str,
    materialization_state: PackMemberMaterializationState,
    override_kind: PackMemberOverrideKind,
    tx: &mut Transaction<'_, Sqlite>,
) -> crate::Result<bool> {
    let result = sqlx::query(
        "UPDATE instance_pack_members
         SET materialization_state = ?, override_kind = ?, modified_at = ?
         WHERE id = ?",
    )
    .bind(materialization_state.as_str())
    .bind(override_kind.as_str())
    .bind(Utc::now().timestamp())
    .bind(member_id)
    .execute(&mut **tx)
    .await?;
    Ok(result.rows_affected() > 0)
}

pub(crate) async fn get_pending_manual_downloads(
    instance_id: &str,
    pool: &SqlitePool,
) -> crate::Result<Vec<PendingManualDownload>> {
    let rows = sqlx::query_as::<_, PendingManualDownloadRow>(
        "SELECT * FROM instance_pending_manual_downloads
         WHERE instance_id = ? AND state IN ('waiting', 'matched', 'error')
         ORDER BY created_at ASC, id ASC",
    )
    .bind(instance_id)
    .fetch_all(pool)
    .await?;

    rows.into_iter().map(TryInto::try_into).collect()
}

/// Restores membership for legacy pack-owned files without inventing remote
/// identity or treating their current bytes as the pack's expected artifact.
/// Entry identity keeps missing and disabled files at the same path distinct.
pub(crate) async fn repair_pack_members_in_transaction(
    content_set_id: &str,
    tx: &mut Transaction<'_, Sqlite>,
) -> crate::Result<bool> {
    let result = sqlx::query(
        "INSERT INTO instance_pack_members (
            id, content_set_id, content_entry_id, member_key, project_type,
            expected_relative_path, required, materialization_state,
            override_kind, reconciled, created_at, modified_at
         ) SELECT 'pack-member:entry:' || entry.id, entry.content_set_id, entry.id,
            'entry:' || entry.id, entry.project_type,
            file.pack_path, 1,
            CASE WHEN file.missing = 1 THEN 'missing' ELSE 'present' END,
            CASE WHEN file.enabled = 0 THEN 'disabled' ELSE 'none' END,
            0, entry.added_at, entry.modified_at
         FROM instance_content_entries entry
         JOIN (
            SELECT *, CASE WHEN substr(relative_path, -9) = '.disabled'
                THEN substr(relative_path, 1, length(relative_path) - 9)
                ELSE relative_path END AS pack_path
            FROM instance_files
         ) file ON file.id = entry.file_id
         WHERE entry.content_set_id = ? AND entry.ownership_kind = 'pack_managed'
            AND NOT EXISTS (
                SELECT 1 FROM instance_pack_members member
                WHERE member.content_set_id = entry.content_set_id
                    AND member.content_entry_id = entry.id
            )",
    )
    .bind(content_set_id)
    .execute(&mut **tx)
    .await?;
    Ok(result.rows_affected() != 0)
}

pub(crate) async fn upsert_pack_member_in_transaction(
    member: &PackMember,
    tx: &mut Transaction<'_, Sqlite>,
) -> crate::Result<()> {
    sqlx::query(
        "INSERT INTO instance_pack_members (
            id, content_set_id, content_entry_id, member_key, project_type,
            expected_relative_path, provider, provider_project_id,
            provider_release_id, required, expected_sha1, expected_size,
            expected_fingerprint, materialization_state, override_kind,
            reconciled, created_at, modified_at
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(content_set_id, member_key) DO UPDATE SET
            content_entry_id = excluded.content_entry_id,
            project_type = excluded.project_type,
            expected_relative_path = excluded.expected_relative_path,
            provider = excluded.provider,
            provider_project_id = excluded.provider_project_id,
            provider_release_id = excluded.provider_release_id,
            required = CASE
                WHEN instance_pack_members.reconciled = 0 THEN excluded.required
                ELSE instance_pack_members.required
            END,
            expected_sha1 = excluded.expected_sha1,
            expected_size = excluded.expected_size,
            expected_fingerprint = excluded.expected_fingerprint,
            materialization_state = excluded.materialization_state,
            override_kind = CASE
                WHEN excluded.override_kind = 'none' THEN override_kind
                ELSE excluded.override_kind
            END,
            reconciled = excluded.reconciled,
            modified_at = excluded.modified_at",
    )
    .bind(&member.id)
    .bind(&member.content_set_id)
    .bind(&member.content_entry_id)
    .bind(&member.member_key)
    .bind(member.project_type.get_name())
    .bind(&member.expected_relative_path)
    .bind(member.provider.map(ContentProvider::as_str))
    .bind(&member.provider_project_id)
    .bind(&member.provider_release_id)
    .bind(i64::from(member.required))
    .bind(&member.expected_sha1)
    .bind(member.expected_size.map(|value| value as i64))
    .bind(member.expected_fingerprint.map(|value| value as i64))
    .bind(member.materialization_state.as_str())
    .bind(member.override_kind.as_str())
    .bind(i64::from(member.reconciled))
    .bind(member.created_at.timestamp())
    .bind(member.modified_at.timestamp())
    .execute(&mut **tx)
    .await?;

    Ok(())
}

pub(crate) async fn upsert_pending_manual_download_in_transaction(
    download: &PendingManualDownload,
    tx: &mut Transaction<'_, Sqlite>,
) -> crate::Result<()> {
    sqlx::query(
        "INSERT INTO instance_pending_manual_downloads (
            id, instance_id, pack_member_id, content_entry_id,
            operation_kind, operation_target_id, project_type, provider,
            provider_project_id, provider_release_id, file_name, website_url,
            target_relative_path, expected_sha1, expected_size,
            expected_fingerprint, state, context, created_at, modified_at
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(instance_id, operation_kind, provider,
            provider_project_id, provider_release_id) WHERE state IN ('waiting', 'matched')
         DO UPDATE SET
            pack_member_id = excluded.pack_member_id,
            content_entry_id = excluded.content_entry_id,
            file_name = excluded.file_name,
            website_url = excluded.website_url,
            target_relative_path = excluded.target_relative_path,
            expected_sha1 = excluded.expected_sha1,
            expected_size = excluded.expected_size,
            expected_fingerprint = excluded.expected_fingerprint,
            context = excluded.context,
            modified_at = excluded.modified_at",
    )
    .bind(&download.id)
    .bind(&download.instance_id)
    .bind(&download.pack_member_id)
    .bind(&download.content_entry_id)
    .bind(download.operation_kind.as_str())
    .bind(&download.operation_target_id)
    .bind(download.project_type.get_name())
    .bind(download.provider.as_str())
    .bind(&download.provider_project_id)
    .bind(&download.provider_release_id)
    .bind(&download.file_name)
    .bind(&download.website_url)
    .bind(&download.target_relative_path)
    .bind(&download.expected_sha1)
    .bind(download.expected_size.map(|value| value as i64))
    .bind(download.expected_fingerprint.map(|value| value as i64))
    .bind(download.state.as_str())
    .bind(download.context.to_string())
    .bind(download.created_at.timestamp())
    .bind(download.modified_at.timestamp())
    .execute(&mut **tx)
    .await?;

    Ok(())
}

pub(crate) async fn complete_pending_manual_download(
    instance_id: &str,
    provider_project_id: &str,
    provider_release_id: &str,
    content_entry_id: Option<&str>,
    tx: &mut Transaction<'_, Sqlite>,
) -> crate::Result<()> {
    sqlx::query(
        "UPDATE instance_pending_manual_downloads
         SET state = 'imported', content_entry_id = ?, modified_at = ?
         WHERE instance_id = ? AND provider = 'curseforge'
            AND provider_project_id = ? AND provider_release_id = ?
            AND state IN ('waiting', 'matched', 'error')",
    )
    .bind(content_entry_id)
    .bind(Utc::now().timestamp())
    .bind(instance_id)
    .bind(provider_project_id)
    .bind(provider_release_id)
    .execute(&mut **tx)
    .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{ModrinthProjectId, ModrinthVersionId};
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

        let materialized =
            get_materialized_curseforge_downloads("target", &pool)
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
}
