//! The launcher's device token for the Xbox Live service, kept as one row.
//!
//! The whole token is kept in the system credential store as one secret, and the
//! row's `secrets_externalized` says whether the store holds it; a token the store
//! would not take stays in the row.

use serde::{Deserialize, Serialize};
use sqlx::Row;
use sqlx::SqliteConnection;
use sqlx::sqlite::SqliteRow;

use super::Collection;
use super::secrets::{self, Stored};

/// The device token, kept once for the whole launcher.
pub(crate) struct MinecraftDeviceTokens;

/// There is one device token per launcher, so its secret is stored under the one
/// key that names it.
fn token_key() -> String {
    secrets::key(&["minecraft-device-token"])
}

/// The bit of `secrets_externalized` that says the store holds the token.
const TOKEN_KEPT: i64 = 1;

/// There is one device token per launcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Singleton;

/// What a device token carries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct MinecraftDeviceTokenRecord {
    pub uuid: String,
    pub private_key: String,
    pub x: String,
    pub y: String,
    pub issue_instant: i64,
    pub not_after: i64,
    pub token: String,
    /// The claims as the JSON text they are stored as.
    pub display_claims: String,
}

impl MinecraftDeviceTokenRecord {
    /// What the row holds once the token is in the store: the columns keep the
    /// shape their type demands and nothing of the token.
    fn cleared() -> Self {
        Self {
            uuid: String::new(),
            private_key: String::new(),
            x: String::new(),
            y: String::new(),
            issue_instant: 0,
            not_after: 0,
            token: String::new(),
            display_claims: "{}".to_string(),
        }
    }
}

impl Collection for MinecraftDeviceTokens {
    type Key = Singleton;
    type Scope = Singleton;
    type Record = MinecraftDeviceTokenRecord;

    fn in_scope(_scope: &Self::Scope, _key: &Self::Key) -> bool {
        true
    }

    async fn read(
        conn: &mut SqliteConnection,
        _key: &Self::Key,
    ) -> crate::Result<Option<Self::Record>> {
        let row = sqlx::query(
            "
            SELECT
                uuid, private_key, x, y, issue_instant, not_after, token,
                json(display_claims) AS display_claims, secrets_externalized
            FROM minecraft_device_tokens
            WHERE id = 0
            ",
        )
        .fetch_optional(&mut *conn)
        .await?;

        let Some(row) = row else {
            return Ok(None);
        };

        let marks: i64 = row.try_get("secrets_externalized")?;
        if marks & TOKEN_KEPT == 0 {
            return Ok(Some(to_record(row)?));
        }

        Ok(Some(match secrets::take(token_key()).await {
            Stored::Value(secret) => match serde_json::from_str::<
                MinecraftDeviceTokenRecord,
            >(&secret)
            {
                Ok(record) => record,
                Err(error) => {
                    tracing::warn!(
                        %error,
                        "The stored device token cannot be read; the row is used instead"
                    );
                    to_record(row)?
                }
            },
            Stored::Missing | Stored::Unavailable => to_record(row)?,
        }))
    }

    async fn write(
        conn: &mut SqliteConnection,
        _key: &Self::Key,
        record: Option<&Self::Record>,
    ) -> crate::Result<()> {
        let Some(record) = record else {
            sqlx::query("DELETE FROM minecraft_device_tokens WHERE id = 0")
                .execute(&mut *conn)
                .await?;
            secrets::forget(token_key()).await;

            return Ok(());
        };

        write_row(conn, record).await
    }

    async fn read_all(
        conn: &mut SqliteConnection,
    ) -> crate::Result<Vec<(Self::Key, Self::Record)>> {
        Ok(Self::read(conn, &Singleton)
            .await?
            .map(|record| vec![(Singleton, record)])
            .unwrap_or_default())
    }

    async fn replace_all(
        conn: &mut SqliteConnection,
        records: &[(Self::Key, Self::Record)],
    ) -> crate::Result<()> {
        Self::write(conn, &Singleton, records.first().map(|(_, record)| record))
            .await
    }

    async fn read_scoped(
        conn: &mut SqliteConnection,
        scope: &Self::Scope,
    ) -> crate::Result<Vec<(Self::Key, Self::Record)>> {
        Ok(Self::read(conn, scope)
            .await?
            .map(|record| vec![(*scope, record)])
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

fn to_record(row: SqliteRow) -> crate::Result<MinecraftDeviceTokenRecord> {
    Ok(MinecraftDeviceTokenRecord {
        uuid: row.try_get("uuid")?,
        private_key: row.try_get("private_key")?,
        x: row.try_get("x")?,
        y: row.try_get("y")?,
        issue_instant: row.try_get("issue_instant")?,
        not_after: row.try_get("not_after")?,
        token: row.try_get("token")?,
        display_claims: row.try_get("display_claims")?,
    })
}

async fn write_row(
    conn: &mut SqliteConnection,
    record: &MinecraftDeviceTokenRecord,
) -> crate::Result<()> {
    // The store is written first, so the row only says the token was moved once
    // the token is there.
    let stored =
        secrets::keep(token_key(), &serde_json::to_string(record)?).await;

    write_columns(conn, record, stored).await
}

async fn write_columns(
    conn: &mut SqliteConnection,
    record: &MinecraftDeviceTokenRecord,
    stored: bool,
) -> crate::Result<()> {
    let written = if stored {
        MinecraftDeviceTokenRecord::cleared()
    } else {
        record.clone()
    };

    sqlx::query(
        "
        INSERT INTO minecraft_device_tokens (
            id, uuid, private_key, x, y, issue_instant, not_after, token,
            display_claims, secrets_externalized
        )
        VALUES (0, ?, ?, ?, ?, ?, ?, ?, jsonb(?), ?)
        ON CONFLICT (id) DO UPDATE SET
            uuid = excluded.uuid,
            private_key = excluded.private_key,
            x = excluded.x,
            y = excluded.y,
            issue_instant = excluded.issue_instant,
            not_after = excluded.not_after,
            token = excluded.token,
            display_claims = excluded.display_claims,
            secrets_externalized = excluded.secrets_externalized
        ",
    )
    .bind(&written.uuid)
    .bind(&written.private_key)
    .bind(&written.x)
    .bind(&written.y)
    .bind(written.issue_instant)
    .bind(written.not_after)
    .bind(&written.token)
    .bind(&written.display_claims)
    .bind(if stored { TOKEN_KEPT } else { 0 })
    .execute(&mut *conn)
    .await?;

    Ok(())
}

/// Moves the token this row still carries into the credential store, and clears
/// the row once the token is there.
///
/// Runs once at startup, and again after any run that could not move the row. A
/// row whose token is in the store has nothing left to move, so a second pass has
/// nothing to do.
pub(crate) async fn externalize(
    conn: &mut SqliteConnection,
) -> crate::Result<()> {
    let pending = sqlx::query(
        "
        SELECT
            uuid, private_key, x, y, issue_instant, not_after, token,
            json(display_claims) AS display_claims
        FROM minecraft_device_tokens
        WHERE id = 0 AND secrets_externalized = 0
          AND (uuid <> '' OR private_key <> '' OR token <> '')
        ",
    )
    .fetch_optional(&mut *conn)
    .await?;

    if let Some(row) = pending {
        let record = to_record(row)?;
        let stored =
            secrets::keep(token_key(), &serde_json::to_string(&record)?).await;
        if stored {
            write_columns(conn, &record, true).await?;
        } else if secrets::unavailable() {
            return Ok(());
        }
    }

    reconcile(conn).await
}

/// A row that says its token was moved but whose entry is gone holds no token at
/// all: its mark is cleared, so the row reads as one that needs a new device
/// registration instead of failing.
async fn reconcile(conn: &mut SqliteConnection) -> crate::Result<()> {
    let marked: bool = sqlx::query_scalar(
        "SELECT EXISTS(
            SELECT 1 FROM minecraft_device_tokens
            WHERE id = 0 AND secrets_externalized <> 0
        )",
    )
    .fetch_one(&mut *conn)
    .await?;
    if !marked {
        return Ok(());
    }

    match secrets::take(token_key()).await {
        Stored::Value(_) | Stored::Unavailable => Ok(()),
        Stored::Missing => {
            sqlx::query(
                "UPDATE minecraft_device_tokens SET secrets_externalized = 0
                 WHERE id = 0",
            )
            .execute(&mut *conn)
            .await?;

            Ok(())
        }
    }
}
