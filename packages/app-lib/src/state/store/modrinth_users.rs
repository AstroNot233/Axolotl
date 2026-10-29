//! The Modrinth account the launcher is signed in with.
//!
//! The session id is kept in the system credential store, and the row's
//! `secrets_externalized` says whether the store holds it; a session the store
//! would not take stays in its column.

use sqlx::SqliteConnection;

use super::Collection;
use super::secrets::{self, Stored};

/// The Modrinth sessions, one per account.
pub(crate) struct ModrinthUsers;

/// The bit of `secrets_externalized` that says the store holds the session id.
const SESSION_ID_KEPT: i64 = 1;

/// Where one session's id is stored.
fn session_key(id: &str) -> String {
    secrets::key(&["modrinth-user", id, "session_id"])
}

/// A session is identified by the account it belongs to.
pub(crate) type ModrinthUserKey = String;

/// Which sessions a read wants: the active one, or the ones that are not.
pub(crate) type ModrinthUserScope = bool;

/// What a session carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModrinthUserRecord {
    pub active: bool,
    pub session_id: String,
    pub expires: i64,
}

impl Collection for ModrinthUsers {
    type Key = ModrinthUserKey;
    type Scope = ModrinthUserScope;
    type Record = ModrinthUserRecord;

    /// The state a session is in lives in the record, so the collection cannot
    /// tell from the account id alone whether it is the active one.
    fn in_scope(_scope: &Self::Scope, _key: &Self::Key) -> bool {
        true
    }

    async fn read(
        conn: &mut SqliteConnection,
        key: &Self::Key,
    ) -> crate::Result<Option<Self::Record>> {
        let row: Option<(bool, String, i64, i64)> = sqlx::query_as(
            "SELECT active, session_id, expires, secrets_externalized
             FROM modrinth_users
             WHERE id = ?",
        )
        .bind(key)
        .fetch_optional(&mut *conn)
        .await?;

        let Some((active, session_id, expires, marks)) = row else {
            return Ok(None);
        };

        Ok(Some(Self::Record {
            active,
            session_id: secrets::resolve(
                session_key(key),
                marks & SESSION_ID_KEPT != 0,
                session_id,
            )
            .await,
            expires,
        }))
    }

    async fn write(
        conn: &mut SqliteConnection,
        key: &Self::Key,
        record: Option<&Self::Record>,
    ) -> crate::Result<()> {
        let Some(record) = record else {
            sqlx::query("DELETE FROM modrinth_users WHERE id = ?")
                .bind(key)
                .execute(&mut *conn)
                .await?;
            secrets::forget(session_key(key)).await;

            return Ok(());
        };

        write_row(conn, key, record).await
    }

    async fn read_all(
        conn: &mut SqliteConnection,
    ) -> crate::Result<Vec<(Self::Key, Self::Record)>> {
        let rows: Vec<(String, bool, String, i64, i64)> = sqlx::query_as(
            "SELECT id, active, session_id, expires, secrets_externalized
             FROM modrinth_users",
        )
        .fetch_all(&mut *conn)
        .await?;

        let mut sessions = Vec::with_capacity(rows.len());
        for (id, active, session_id, expires, marks) in rows {
            sessions.push((
                id.clone(),
                Self::Record {
                    active,
                    session_id: secrets::resolve(
                        session_key(&id),
                        marks & SESSION_ID_KEPT != 0,
                        session_id,
                    )
                    .await,
                    expires,
                },
            ));
        }

        Ok(sessions)
    }

    async fn replace_all(
        conn: &mut SqliteConnection,
        records: &[(Self::Key, Self::Record)],
    ) -> crate::Result<()> {
        ensure_one_active(records)?;
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
        let rows: Vec<(String, bool, String, i64, i64)> = sqlx::query_as(
            "SELECT id, active, session_id, expires, secrets_externalized
             FROM modrinth_users
             WHERE active = ?",
        )
        .bind(*scope)
        .fetch_all(&mut *conn)
        .await?;

        let mut sessions = Vec::with_capacity(rows.len());
        for (id, active, session_id, expires, marks) in rows {
            sessions.push((
                id.clone(),
                Self::Record {
                    active,
                    session_id: secrets::resolve(
                        session_key(&id),
                        marks & SESSION_ID_KEPT != 0,
                        session_id,
                    )
                    .await,
                    expires,
                },
            ));
        }

        Ok(sessions)
    }

    async fn replace_scoped(
        conn: &mut SqliteConnection,
        scope: &Self::Scope,
        records: &[(Self::Key, Self::Record)],
    ) -> crate::Result<()> {
        ensure_one_active(records)?;
        ensure_records_are_in_scope(scope, records)?;
        ensure_keys_belong_to_scope(conn, scope, records).await?;
        delete_omitted(conn, Some(*scope), records).await?;
        for (key, record) in records {
            write_row(conn, key, record).await?;
        }
        Ok(())
    }
}

/// A session written into a scope is in the state that scope names: writing a
/// signed-in session into the idle group would clear the others and then take
/// the session away from where it belongs.
fn ensure_records_are_in_scope(
    scope: &ModrinthUserScope,
    records: &[(ModrinthUserKey, ModrinthUserRecord)],
) -> crate::Result<()> {
    for (key, record) in records {
        if record.active != *scope {
            return Err(crate::ErrorKind::InputError(format!(
                "Session {key} was written into a state it is not in"
            ))
            .into());
        }
    }

    Ok(())
}

/// A replacement writes by account, so a session stored in the other state
/// would be moved rather than replaced: its row is deleted here and rewritten
/// there, taking the signed-in session with it.
async fn ensure_keys_belong_to_scope(
    conn: &mut SqliteConnection,
    scope: &ModrinthUserScope,
    records: &[(ModrinthUserKey, ModrinthUserRecord)],
) -> crate::Result<()> {
    for (key, _) in records {
        let Some(existing) = ModrinthUsers::read(conn, key).await? else {
            continue;
        };
        if existing.active != *scope {
            return Err(crate::ErrorKind::InputError(format!(
                "Session {key} belongs to the other state"
            ))
            .into());
        }
    }

    Ok(())
}

/// The table carries one active session; it no longer has an index that says so,
/// so every write that marks a session active clears the others first.
async fn clear_other_active(
    conn: &mut SqliteConnection,
    key: &str,
) -> crate::Result<()> {
    sqlx::query("UPDATE modrinth_users SET active = 0 WHERE id <> ?")
        .bind(key)
        .execute(&mut *conn)
        .await?;

    Ok(())
}

/// At most one of the records a replacement carries may be the active one.
fn ensure_one_active(
    records: &[(ModrinthUserKey, ModrinthUserRecord)],
) -> crate::Result<()> {
    let active = records.iter().filter(|(_, record)| record.active).count();
    if active > 1 {
        return Err(crate::ErrorKind::InputError(
            "More than one Modrinth session was written as the active one"
                .to_string(),
        )
        .into());
    }

    Ok(())
}

async fn write_row(
    conn: &mut SqliteConnection,
    key: &str,
    record: &ModrinthUserRecord,
) -> crate::Result<()> {
    if record.active {
        clear_other_active(conn, key).await?;
    }

    // The store is written first, so a row only says the session was moved once
    // the session is there. A session the store would not take is written to the
    // column instead, and reads back the way it did before the move.
    let stored = !record.session_id.is_empty()
        && secrets::keep(session_key(key), &record.session_id).await;
    let (session_id, marks) = if stored {
        ("", SESSION_ID_KEPT)
    } else {
        (record.session_id.as_str(), 0)
    };
    if record.session_id.is_empty() {
        secrets::forget(session_key(key)).await;
    }

    sqlx::query(
        "
        INSERT INTO modrinth_users (
            id, active, session_id, expires, secrets_externalized
        )
        VALUES (?, ?, ?, ?, ?)
        ON CONFLICT (id) DO UPDATE SET
            active = excluded.active,
            session_id = excluded.session_id,
            expires = excluded.expires,
            secrets_externalized = excluded.secrets_externalized
        ",
    )
    .bind(key)
    .bind(record.active)
    .bind(session_id)
    .bind(record.expires)
    .bind(marks)
    .execute(&mut *conn)
    .await?;

    Ok(())
}

/// Removes the sessions a replacement leaves out, and only those.
async fn delete_omitted(
    conn: &mut SqliteConnection,
    scope: Option<ModrinthUserScope>,
    records: &[(ModrinthUserKey, ModrinthUserRecord)],
) -> crate::Result<()> {
    let existing: Vec<String> = match scope {
        Some(active) => {
            sqlx::query_scalar("SELECT id FROM modrinth_users WHERE active = ?")
                .bind(active)
                .fetch_all(&mut *conn)
                .await?
        }
        None => {
            sqlx::query_scalar("SELECT id FROM modrinth_users")
                .fetch_all(&mut *conn)
                .await?
        }
    };

    for id in existing {
        if records.iter().any(|(key, _)| key == &id) {
            continue;
        }
        sqlx::query("DELETE FROM modrinth_users WHERE id = ?")
            .bind(&id)
            .execute(&mut *conn)
            .await?;
        secrets::forget(session_key(&id)).await;
    }

    Ok(())
}

/// Moves the session ids these rows still carry into the credential store, and
/// clears the column whose id is there.
///
/// Runs once at startup, and again after any run that could not move a row. A row
/// whose session is in the store has nothing left in its column, so a second pass
/// has nothing to do.
pub(crate) async fn externalize(
    conn: &mut SqliteConnection,
) -> crate::Result<()> {
    let pending: Vec<(String, String)> = sqlx::query_as(
        "SELECT id, session_id
         FROM modrinth_users
         WHERE secrets_externalized = 0 AND session_id <> ''",
    )
    .fetch_all(&mut *conn)
    .await?;

    for (id, session_id) in pending {
        if !secrets::keep(session_key(&id), &session_id).await {
            if secrets::unavailable() {
                return Ok(());
            }
            continue;
        }

        sqlx::query(
            "UPDATE modrinth_users
             SET session_id = '', secrets_externalized = ?
             WHERE id = ?",
        )
        .bind(SESSION_ID_KEPT)
        .bind(&id)
        .execute(&mut *conn)
        .await?;
    }

    reconcile(conn).await
}

/// A row that says its session was moved but whose entry is gone holds no session
/// at all: its mark is cleared, so it reads as a signed-out account instead of
/// failing, and the next write moves the session again.
async fn reconcile(conn: &mut SqliteConnection) -> crate::Result<()> {
    let marked: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM modrinth_users WHERE secrets_externalized <> 0",
    )
    .fetch_all(&mut *conn)
    .await?;

    for id in marked {
        match secrets::take(session_key(&id)).await {
            Stored::Value(_) => {}
            Stored::Missing => {
                sqlx::query(
                    "UPDATE modrinth_users SET secrets_externalized = 0
                     WHERE id = ?",
                )
                .bind(&id)
                .execute(&mut *conn)
                .await?;
            }
            Stored::Unavailable => return Ok(()),
        }
    }

    Ok(())
}
