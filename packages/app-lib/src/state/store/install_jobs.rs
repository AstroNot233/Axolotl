//! An install job: what it was asked to do, how far it got, and its state.

use sqlx::SqliteConnection;

use super::Collection;

/// The jobs, one per installation run.
pub(crate) struct InstallJobs;

/// A job is identified by its own id.
pub(crate) type InstallJobKey = String;

/// Which jobs a read wants: those in one status, or those of one instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InstallJobScope {
    Status(String),
    Instance(String),
}

/// What a job carries.
///
/// The progress a job has made is kept as the text it is stored as: what the
/// text says is the caller's business.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InstallJobRecord {
    pub instance_id: Option<String>,
    pub kind: String,
    pub status: String,
    pub state: String,
    pub created: i64,
    pub modified: i64,
    pub finished: Option<i64>,
    pub dismissed: bool,
}

impl Collection for InstallJobs {
    type Key = InstallJobKey;
    type Scope = InstallJobScope;
    type Record = InstallJobRecord;

    /// A job's status changes under it, so the collection cannot tell from the
    /// key whether a job is in a given scope: what decides that is the record,
    /// and the replacement checks each record against the scope before it
    /// writes anything.
    fn in_scope(_scope: &Self::Scope, _key: &Self::Key) -> bool {
        true
    }

    async fn read(
        conn: &mut SqliteConnection,
        key: &Self::Key,
    ) -> crate::Result<Option<Self::Record>> {
        let row: Option<JobRow> = sqlx::query_as(
            "
            SELECT instance_id, kind, status, state, created, modified,
                finished, dismissed
            FROM install_jobs
            WHERE id = ?
            ",
        )
        .bind(key)
        .fetch_optional(&mut *conn)
        .await?;

        Ok(row.map(to_record))
    }

    async fn write(
        conn: &mut SqliteConnection,
        key: &Self::Key,
        record: Option<&Self::Record>,
    ) -> crate::Result<()> {
        let Some(record) = record else {
            sqlx::query("DELETE FROM install_jobs WHERE id = ?")
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
        let rows: Vec<(String, JobRow)> = sqlx::query_as(
            "
            SELECT id, instance_id, kind, status, state, created, modified,
                finished, dismissed
            FROM install_jobs
            ",
        )
        .fetch_all(&mut *conn)
        .await?;

        Ok(rows
            .into_iter()
            .map(|(id, row)| (id, to_record(row)))
            .collect())
    }

    async fn replace_all(
        conn: &mut SqliteConnection,
        records: &[(Self::Key, Self::Record)],
    ) -> crate::Result<()> {
        delete_omitted(conn, None, records).await?;
        for (key, record) in records {
            write_row(conn, key, record).await?;
        }
        Ok(())
    }

    async fn read_scoped(
        conn: &mut SqliteConnection,
        scope: &Self::Scope,
    ) -> crate::Result<Vec<(Self::Key, Self::Record)>> {
        let rows: Vec<(String, JobRow)> = match scope {
            InstallJobScope::Status(status) => {
                sqlx::query_as(
                    "
                    SELECT id, instance_id, kind, status, state, created,
                        modified, finished, dismissed
                    FROM install_jobs
                    WHERE status = ?
                    ",
                )
                .bind(status)
                .fetch_all(&mut *conn)
                .await?
            }
            InstallJobScope::Instance(instance_id) => {
                sqlx::query_as(
                    "
                    SELECT id, instance_id, kind, status, state, created,
                        modified, finished, dismissed
                    FROM install_jobs
                    WHERE instance_id = ?
                    ",
                )
                .bind(instance_id)
                .fetch_all(&mut *conn)
                .await?
            }
        };

        Ok(rows
            .into_iter()
            .map(|(id, row)| (id, to_record(row)))
            .collect())
    }

    async fn replace_scoped(
        conn: &mut SqliteConnection,
        scope: &Self::Scope,
        records: &[(Self::Key, Self::Record)],
    ) -> crate::Result<()> {
        ensure_records_are_in_scope(scope, records)?;
        delete_omitted(conn, Some(scope), records).await?;
        for (key, record) in records {
            write_row(conn, key, record).await?;
        }
        Ok(())
    }
}

type JobRow = (
    Option<String>,
    String,
    String,
    String,
    i64,
    i64,
    Option<i64>,
    bool,
);

fn to_record(row: JobRow) -> InstallJobRecord {
    let (
        instance_id,
        kind,
        status,
        state,
        created,
        modified,
        finished,
        dismissed,
    ) = row;
    InstallJobRecord {
        instance_id,
        kind,
        status,
        state,
        created,
        modified,
        finished,
        dismissed,
    }
}

async fn write_row(
    conn: &mut SqliteConnection,
    key: &str,
    record: &InstallJobRecord,
) -> crate::Result<()> {
    sqlx::query(
        "
        INSERT INTO install_jobs (
            id, instance_id, kind, status, state, created, modified,
            finished, dismissed
        )
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT (id) DO UPDATE SET
            instance_id = excluded.instance_id,
            kind = excluded.kind,
            status = excluded.status,
            state = excluded.state,
            created = excluded.created,
            modified = excluded.modified,
            finished = excluded.finished,
            dismissed = excluded.dismissed
        ",
    )
    .bind(key)
    .bind(&record.instance_id)
    .bind(&record.kind)
    .bind(&record.status)
    .bind(&record.state)
    .bind(record.created)
    .bind(record.modified)
    .bind(record.finished)
    .bind(record.dismissed)
    .execute(&mut *conn)
    .await?;

    Ok(())
}

/// Removes the jobs a replacement leaves out, and only those: a job the
/// replacement keeps has to survive, because the plan it was started from hangs
/// off it.
async fn delete_omitted(
    conn: &mut SqliteConnection,
    scope: Option<&String>,
    records: &[(InstallJobKey, InstallJobRecord)],
) -> crate::Result<()> {
    let existing: Vec<(String, String)> = match scope {
        Some(status) => {
            sqlx::query_as(
                "SELECT id, status FROM install_jobs WHERE status = ?",
            )
            .bind(status)
            .fetch_all(&mut *conn)
            .await?
        }
        None => {
            sqlx::query_as("SELECT id, status FROM install_jobs")
                .fetch_all(&mut *conn)
                .await?
        }
    };

    for (id, _) in existing {
        if records.iter().any(|(key, _)| key == &id) {
            continue;
        }
        sqlx::query("DELETE FROM install_jobs WHERE id = ?")
            .bind(&id)
            .execute(&mut *conn)
            .await?;
    }

    Ok(())
}

/// A scoped replacement may only touch the jobs of that scope, and the jobs it
/// writes must be in it: a job whose status differs from the status being
/// replaced would move another group, and one that names another instance would
/// move between instances.
fn ensure_records_are_in_scope(
    scope: &InstallJobScope,
    records: &[(InstallJobKey, InstallJobRecord)],
) -> crate::Result<()> {
    for (key, record) in records {
        let in_scope = match scope {
            InstallJobScope::Status(status) => &record.status == status,
            InstallJobScope::Instance(instance_id) => {
                record.instance_id.as_deref() == Some(instance_id)
            }
        };
        if !in_scope {
            return Err(crate::ErrorKind::InputError(format!(
                "Install job {key} is not among the jobs of the scope it was written to"
            ))
            .into());
        }
    }

    Ok(())
}
