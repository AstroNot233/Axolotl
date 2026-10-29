//! What the account secrets promise: which columns a row keeps once the system
//! credential store takes the secret, and what stays in the row when it does not.

use sqlx::SqlitePool;

use super::support;
use crate::state::store::Collection as _;
use crate::state::store::Handle;
use crate::state::store::minecraft_device_tokens::{
    MinecraftDeviceTokenRecord, MinecraftDeviceTokens, Singleton,
};
use crate::state::store::minecraft_users::{
    AccountType, MinecraftUserRecord, MinecraftUsers,
};
use crate::state::store::modrinth_users::{ModrinthUserRecord, ModrinthUsers};
use crate::state::store::secrets;

fn account(access_token: &str, refresh_token: &str) -> MinecraftUserRecord {
    MinecraftUserRecord {
        uuid: "uuid".to_string(),
        active: false,
        username: "Player".to_string(),
        account_type: AccountType::Microsoft,
        access_token: access_token.to_string(),
        refresh_token: refresh_token.to_string(),
        expires: 0,
        yggdrasil_api_root: String::new(),
        yggdrasil_server_name: String::new(),
        yggdrasil_login: String::new(),
        yggdrasil_client_token: String::new(),
    }
}

fn session() -> ModrinthUserRecord {
    ModrinthUserRecord {
        active: true,
        session_id: "session".to_string(),
        expires: 10,
    }
}

fn device_token() -> MinecraftDeviceTokenRecord {
    MinecraftDeviceTokenRecord {
        uuid: "device".to_string(),
        private_key: "private-key".to_string(),
        x: "1".to_string(),
        y: "2".to_string(),
        issue_instant: 10,
        not_after: 20,
        token: "token".to_string(),
        display_claims: r#"{"xui":[{"uhs":"1"}]}"#.to_string(),
    }
}

fn account_tokens_key(account_id: &str, column: &str) -> String {
    secrets::key(&["minecraft-user", account_id, column])
}

/// The account's own columns, as the database holds them.
async fn account_row(
    pool: &SqlitePool,
    account_id: &str,
) -> (String, String, i64) {
    sqlx::query_as(
        "SELECT access_token, refresh_token, secrets_externalized
         FROM minecraft_users
         WHERE account_id = ?",
    )
    .bind(account_id)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn session_row(pool: &SqlitePool, id: &str) -> (String, i64) {
    sqlx::query_as(
        "SELECT session_id, secrets_externalized FROM modrinth_users WHERE id = ?",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn device_row(pool: &SqlitePool) -> (String, String, i64) {
    sqlx::query_as(
        "SELECT token, json(display_claims), secrets_externalized
         FROM minecraft_device_tokens
         WHERE id = 0",
    )
    .fetch_one(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn without_a_store_the_row_keeps_its_tokens() {
    let _host = secrets::testing::without_store();
    let pool = support::pool().await;
    let handle = Handle::new(pool.clone());
    let account_id = "microsoft:no-store".to_string();

    MinecraftUsers::set(
        &handle,
        &account_id,
        Some(&account("access", "refresh")),
    )
    .await
    .unwrap();

    let (access_token, refresh_token, marks) =
        account_row(&pool, &account_id).await;
    assert_eq!(
        (access_token.as_str(), refresh_token.as_str()),
        ("access", "refresh")
    );
    assert_eq!(marks, 0, "a row nothing moved out of carries no mark");

    let read = MinecraftUsers::get(&handle, &account_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(read.access_token, "access");
    assert_eq!(read.refresh_token, "refresh");
}

#[tokio::test]
async fn a_store_holds_the_tokens_and_the_row_is_cleared() {
    let host = secrets::testing::with_store();
    let store = host.store().unwrap();
    let pool = support::pool().await;
    let handle = Handle::new(pool.clone());
    let account_id = "microsoft:one".to_string();

    MinecraftUsers::set(
        &handle,
        &account_id,
        Some(&account("access", "refresh")),
    )
    .await
    .unwrap();
    ModrinthUsers::set(&handle, &"account".to_string(), Some(&session()))
        .await
        .unwrap();
    MinecraftDeviceTokens::set(&handle, &Singleton, Some(&device_token()))
        .await
        .unwrap();

    let (access_token, refresh_token, marks) =
        account_row(&pool, &account_id).await;
    assert_eq!(
        access_token, "",
        "the token column no longer holds the token"
    );
    assert_eq!(refresh_token, "");
    assert_eq!(marks, 0b011, "the two tokens the account has were moved");

    let (session_id, session_marks) = session_row(&pool, "account").await;
    assert_eq!(session_id, "");
    assert_eq!(session_marks, 1);

    let (token, claims, device_marks) = device_row(&pool).await;
    assert_eq!(token, "");
    assert_eq!(claims, "{}");
    assert_eq!(device_marks, 1);

    assert_eq!(
        store
            .value(&account_tokens_key(&account_id, "access_token"))
            .as_deref(),
        Some("access")
    );
    assert_eq!(
        store
            .value(&secrets::key(&["modrinth-user", "account", "session_id"]))
            .as_deref(),
        Some("session")
    );
}

#[tokio::test]
async fn a_read_comes_from_the_store_rather_than_the_row() {
    let host = secrets::testing::with_store();
    let store = host.store().unwrap();
    let pool = support::pool().await;
    let handle = Handle::new(pool.clone());
    let account_id = "microsoft:moved".to_string();

    MinecraftUsers::set(
        &handle,
        &account_id,
        Some(&account("access", "refresh")),
    )
    .await
    .unwrap();
    // The row is empty now, so a read that answers with this is reading the
    // store and nothing else.
    store.put(&account_tokens_key(&account_id, "access_token"), "renewed");

    let read = MinecraftUsers::get(&handle, &account_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(read.access_token, "renewed");
    assert_eq!(read.refresh_token, "refresh");
}

#[tokio::test]
async fn a_secret_longer_than_one_entry_stays_in_the_row() {
    let host = secrets::testing::with_store();
    let store = host.store().unwrap();
    let pool = support::pool().await;
    let handle = Handle::new(pool.clone());
    let account_id = "microsoft:long".to_string();
    // Longer than any credential store entry holds, which is what makes a store
    // shorten or refuse it.
    let access_token = "a".repeat(16384);

    MinecraftUsers::set(
        &handle,
        &account_id,
        Some(&account(&access_token, "refresh")),
    )
    .await
    .unwrap();

    let (stored_access, stored_refresh, marks) =
        account_row(&pool, &account_id).await;
    assert_eq!(stored_access, access_token, "the token was not lost");
    assert_eq!(stored_refresh, "refresh");
    assert_eq!(marks, 0, "a row the store refused is marked as unmoved");
    assert!(
        !store.keys().iter().any(|key| key.contains(&account_id)),
        "and the store keeps none of its tokens"
    );

    let read = MinecraftUsers::get(&handle, &account_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(read.access_token, access_token);
}

#[tokio::test]
async fn moving_the_columns_runs_once_and_leaves_the_rows_as_they_are() {
    let pool = support::pool().await;
    let handle = Handle::new(pool.clone());
    let account_id = "microsoft:secrets-legacy".to_string();
    let session_id = "secrets-session".to_string();

    // An older build wrote these rows with their secrets in place.
    {
        let _host = secrets::testing::without_store();
        MinecraftUsers::set(
            &handle,
            &account_id,
            Some(&account("access", "refresh")),
        )
        .await
        .unwrap();
        ModrinthUsers::set(&handle, &session_id, Some(&session()))
            .await
            .unwrap();
        MinecraftDeviceTokens::set(&handle, &Singleton, Some(&device_token()))
            .await
            .unwrap();
    }

    let host = secrets::testing::with_store();
    let store = host.store().unwrap();
    secrets::externalize(&handle).await.unwrap();

    let (access_token, _, marks) = account_row(&pool, &account_id).await;
    assert_eq!(access_token, "");
    assert_eq!(marks, 0b011);
    assert_eq!(session_row(&pool, &session_id).await, (String::new(), 1));
    let (_, _, device_marks) = device_row(&pool).await;
    assert_eq!(device_marks, 1);
    let written = store.keys().len();
    assert_eq!(
        written, 4,
        "one entry per secret: two tokens, one session and the device token"
    );

    // A second pass finds nothing to move, so it writes nothing at all.
    secrets::externalize(&handle).await.unwrap();

    assert_eq!(store.keys().len(), written, "no second entry was written");
    let (_, _, marks) = account_row(&pool, &account_id).await;
    assert_eq!(marks, 0b011);
    assert_eq!(session_row(&pool, &session_id).await, (String::new(), 1));
    let (_, _, device_marks) = device_row(&pool).await;
    assert_eq!(device_marks, 1);
}
