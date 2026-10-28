use super::instance_rows::{InstanceLinkRow, instance_link_columns};
use crate::state::InstanceLink;
use sqlx::{Executor, Sqlite, Transaction};

pub(crate) async fn get_instance_link<'e, E>(
    instance_id: &str,
    exec: E,
) -> crate::Result<InstanceLink>
where
    E: Executor<'e, Database = Sqlite>,
{
    let row = sqlx::query_as!(
        InstanceLinkRow,
        r#"
        SELECT
            instance_id,
            link_kind,
            modrinth_project_id,
            modrinth_version_id,
            server_project_id,
            content_project_id,
            content_version_id,
            hosting_server_id,
            json(hosting_instance_ids) AS "hosting_instance_ids?: String",
            hosting_active_instance_id,
            shared_instance_id,
            imported_name,
            imported_version_number,
            imported_filename
        FROM instance_links
        WHERE instance_id = ?
        "#,
        instance_id,
    )
    .fetch_optional(exec)
    .await?;

    match row {
        Some(row) => row.try_into(),
        None => Ok(InstanceLink::Unmanaged),
    }
}

pub(crate) async fn upsert_instance_link(
    instance_id: &str,
    link: &InstanceLink,
    tx: &mut Transaction<'_, Sqlite>,
) -> crate::Result<()> {
    let columns = instance_link_columns(link)?;
    if !matches!(link, InstanceLink::Unmanaged) {
        sqlx::query("DELETE FROM resource_pack_unlinks WHERE instance_id = ?")
            .bind(instance_id)
            .execute(&mut **tx)
            .await?;
    }
    let modrinth_project_id = columns.modrinth_project_id.as_deref();
    let modrinth_version_id = columns.modrinth_version_id.as_deref();
    let server_project_id = columns.server_project_id.as_deref();
    let content_project_id = columns.content_project_id.as_deref();
    let content_version_id = columns.content_version_id.as_deref();
    let hosting_server_id = columns.hosting_server_id.as_deref();
    let hosting_instance_ids = columns.hosting_instance_ids.as_deref();
    let hosting_active_instance_id =
        columns.hosting_active_instance_id.as_deref();
    let shared_instance_id = columns.shared_instance_id.as_deref();
    let imported_name = columns.imported_name.as_deref();
    let imported_version_number = columns.imported_version_number.as_deref();
    let imported_filename = columns.imported_filename.as_deref();

    sqlx::query!(
        "
        INSERT INTO instance_links (
            instance_id,
            link_kind,
            modrinth_project_id,
            modrinth_version_id,
            server_project_id,
            content_project_id,
            content_version_id,
            hosting_server_id,
            hosting_instance_ids,
            hosting_active_instance_id,
            shared_instance_id,
            imported_name,
            imported_version_number,
            imported_filename
        )
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, jsonb(?), ?, ?, ?, ?, ?)
        ON CONFLICT (instance_id) DO UPDATE SET
            link_kind = excluded.link_kind,
            modrinth_project_id = excluded.modrinth_project_id,
            modrinth_version_id = excluded.modrinth_version_id,
            server_project_id = excluded.server_project_id,
            content_project_id = excluded.content_project_id,
            content_version_id = excluded.content_version_id,
            hosting_server_id = excluded.hosting_server_id,
            hosting_instance_ids = excluded.hosting_instance_ids,
            hosting_active_instance_id = excluded.hosting_active_instance_id,
            shared_instance_id = excluded.shared_instance_id,
            imported_name = excluded.imported_name,
            imported_version_number = excluded.imported_version_number,
            imported_filename = excluded.imported_filename
        ",
        instance_id,
        columns.link_kind,
        modrinth_project_id,
        modrinth_version_id,
        server_project_id,
        content_project_id,
        content_version_id,
        hosting_server_id,
        hosting_instance_ids,
        hosting_active_instance_id,
        shared_instance_id,
        imported_name,
        imported_version_number,
        imported_filename,
    )
    .execute(&mut **tx)
    .await?;

    Ok(())
}
