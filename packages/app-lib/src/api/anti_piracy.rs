use base64::Engine;
use hmac::{Hmac, Mac};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::time::Duration;
use tokio::sync::Mutex;

use crate::State;

const IP_API_URL: &str = "http://ip-api.com/json/";
const PROOF_KEY: &str = "official-minecraft-login-proof-v1";
const PROOF_MESSAGE: &[u8] = b"axolotl:official-minecraft-login:1:1";
const RESTRICTED_ERROR: &str = "OFFLINE_ACCOUNT_RESTRICTED";
static REGION: AtomicU8 = AtomicU8::new(0);
static SESSION_OFFICIAL_LOGIN: AtomicBool = AtomicBool::new(false);
static PROOF_WRITE: Mutex<()> = Mutex::const_new(());

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Region {
    Checking,
    Cn,
    NonCn,
    Unavailable,
}

impl Region {
    fn current() -> Self {
        match REGION.load(Ordering::Acquire) {
            1 => Self::Cn,
            2 => Self::NonCn,
            3 => Self::Unavailable,
            _ => Self::Checking,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct Status {
    pub region: Region,
    pub restricted: bool,
}

#[derive(Deserialize)]
struct IpApiResponse {
    status: String,
    #[serde(rename = "countryCode")]
    country_code: Option<String>,
}

fn parse_region(body: &str) -> Option<Region> {
    let response: IpApiResponse = serde_json::from_str(body).ok()?;
    if response.status != "success" {
        return None;
    }
    match response.country_code.as_deref() {
        Some("CN") => Some(Region::Cn),
        Some(code)
            if code.len() == 2
                && code.bytes().all(|byte| byte.is_ascii_uppercase()) =>
        {
            Some(Region::NonCn)
        }
        _ => None,
    }
}

async fn fetch_region(
    client: &reqwest::Client,
    url: &str,
    timeout: Duration,
) -> Result<Option<Region>, reqwest::Error> {
    let response = client
        .get(url)
        .timeout(timeout)
        .send()
        .await?
        .error_for_status()?;
    let body = response.text().await?;
    Ok(parse_region(&body))
}

pub async fn check_region() -> Status {
    let region = match State::get().await {
        Ok(state) => {
            match fetch_region(
                &state.configured_http_client(),
                IP_API_URL,
                Duration::from_secs(5),
            )
            .await
            {
                Ok(Some(region)) => region,
                Ok(None) => {
                    tracing::warn!(
                        "IP country lookup returned invalid data; offline accounts remain available"
                    );
                    Region::Unavailable
                }
                Err(error) => {
                    tracing::warn!(%error, "IP country lookup failed; offline accounts remain available");
                    Region::Unavailable
                }
            }
        }
        Err(error) => {
            tracing::warn!(%error, "IP country lookup could not start; offline accounts remain available");
            Region::Unavailable
        }
    };
    REGION.store(
        match region {
            Region::Checking => 0,
            Region::Cn => 1,
            Region::NonCn => 2,
            Region::Unavailable => 3,
        },
        Ordering::Release,
    );
    status().await
}

fn proof_entry() -> Result<keyring::Entry, keyring::Error> {
    keyring::Entry::new(crate::brand::BUNDLE_IDENTIFIER, PROOF_KEY)
}

fn proof_mac(key: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(key)
        .expect("HMAC accepts any key length");
    mac.update(PROOF_MESSAGE);
    base64::engine::general_purpose::STANDARD
        .encode(mac.finalize().into_bytes())
}

fn proof_valid(key: &[u8], stored_mac: &str) -> bool {
    let Ok(stored_mac) =
        base64::engine::general_purpose::STANDARD.decode(stored_mac)
    else {
        return false;
    };
    let mut mac = Hmac::<Sha256>::new_from_slice(key)
        .expect("HMAC accepts any key length");
    mac.update(PROOF_MESSAGE);
    mac.verify_slice(&stored_mac).is_ok()
}

fn restricted(region: Region, has_proof: bool) -> bool {
    region == Region::NonCn && !has_proof
}

async fn has_proof() -> bool {
    let state = match State::get().await {
        Ok(state) => state,
        Err(error) => {
            tracing::warn!(%error, "Cannot read official login proof");
            return false;
        }
    };
    let record = sqlx::query_as::<_, (i64, i64, String)>(
        "SELECT verified, version, mac FROM official_login_proof WHERE id = 0",
    )
    .fetch_optional(&state.pool)
    .await;
    let Some((1, 1, stored_mac)) = (match record {
        Ok(record) => record,
        Err(error) => {
            tracing::warn!(%error, "Cannot read official login proof");
            return false;
        }
    }) else {
        return false;
    };
    let key =
        tokio::task::spawn_blocking(|| proof_entry()?.get_password()).await;
    let Ok(Ok(key)) = key else {
        tracing::warn!("Official login proof key is unavailable");
        return false;
    };
    let Ok(key) = base64::engine::general_purpose::STANDARD.decode(key) else {
        tracing::warn!("Official login proof key is invalid");
        return false;
    };
    if key.len() != 32 {
        tracing::warn!("Official login proof key has an invalid length");
        return false;
    }
    let valid = proof_valid(&key, &stored_mac);
    if !valid {
        tracing::warn!("Official login proof failed integrity verification");
    }
    valid
}

pub async fn status() -> Status {
    let region = Region::current();
    let has_proof = if region == Region::NonCn
        && !SESSION_OFFICIAL_LOGIN.load(Ordering::Acquire)
    {
        has_proof().await
    } else {
        true
    };
    Status {
        region,
        restricted: restricted(region, has_proof),
    }
}

pub async fn ensure_offline_allowed() -> crate::Result<()> {
    if status().await.restricted {
        return Err(crate::ErrorKind::InputError(RESTRICTED_ERROR.to_string())
            .as_error());
    }
    Ok(())
}

pub async fn mark_official_login() -> crate::Result<()> {
    SESSION_OFFICIAL_LOGIN.store(true, Ordering::Release);
    let _guard = PROOF_WRITE.lock().await;
    let state = State::get().await?;
    let key = tokio::task::spawn_blocking(|| {
        let entry = proof_entry()?;
        match entry.get_password() {
            Ok(encoded) => {
                if let Ok(key) =
                    base64::engine::general_purpose::STANDARD.decode(encoded)
                    && key.len() == 32
                {
                    return Ok(key);
                }
            }
            Err(keyring::Error::NoEntry) => {}
            Err(error) => return Err(error),
        }
        let mut key = vec![0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut key);
        entry.set_password(
            &base64::engine::general_purpose::STANDARD.encode(&key),
        )?;
        Ok::<Vec<u8>, keyring::Error>(key)
    })
    .await
    .map_err(|error| {
        crate::ErrorKind::OtherError(error.to_string()).as_error()
    })?
    .map_err(|error| {
        crate::ErrorKind::OtherError(error.to_string()).as_error()
    })?;
    let mac = proof_mac(&key);
    sqlx::query("INSERT INTO official_login_proof (id, verified, version, mac) VALUES (0, 1, 1, ?) ON CONFLICT(id) DO UPDATE SET verified = 1, version = 1, mac = excluded.mac")
        .bind(mac)
        .execute(&state.pool)
        .await?;
    Ok(())
}

pub async fn clear_official_login() -> crate::Result<()> {
    let _guard = PROOF_WRITE.lock().await;
    let state = State::get().await?;
    sqlx::query("DELETE FROM official_login_proof WHERE id = 0")
        .execute(&state.pool)
        .await?;
    SESSION_OFFICIAL_LOGIN.store(false, Ordering::Release);
    let result =
        tokio::task::spawn_blocking(|| proof_entry()?.delete_credential())
            .await
            .map_err(|error| {
                crate::ErrorKind::OtherError(error.to_string()).as_error()
            })?;
    match result {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => {
            tracing::warn!(%error, "Official login proof was cleared, but its key could not be removed");
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::migrate::Migrator;
    use sqlx::sqlite::SqlitePoolOptions;
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;

    #[test]
    fn accepts_only_successful_country_responses() {
        assert_eq!(
            parse_region(r#"{"status":"success","countryCode":"CN"}"#),
            Some(Region::Cn)
        );
        assert_eq!(
            parse_region(r#"{"status":"success","countryCode":"US"}"#),
            Some(Region::NonCn)
        );
        assert_eq!(
            parse_region(r#"{"status":"fail","countryCode":"US"}"#),
            None
        );
        assert_eq!(parse_region(r#"{"status":"success"}"#), None);
        assert_eq!(parse_region("invalid"), None);
    }

    #[test]
    fn proof_mac_is_stable_for_the_same_key() {
        assert_eq!(proof_mac(&[7; 32]), proof_mac(&[7; 32]));
        assert_ne!(proof_mac(&[7; 32]), proof_mac(&[8; 32]));
        assert!(proof_valid(&[7; 32], &proof_mac(&[7; 32])));
        assert!(!proof_valid(&[8; 32], &proof_mac(&[7; 32])));
        assert!(!proof_valid(&[7; 32], "tampered"));
    }

    #[test]
    fn only_explicit_non_cn_without_proof_is_restricted() {
        for region in [Region::Checking, Region::Cn, Region::Unavailable] {
            assert!(!restricted(region, false));
        }
        assert!(restricted(Region::NonCn, false));
        assert!(!restricted(Region::NonCn, true));
    }

    #[tokio::test]
    async fn slow_lookup_times_out_without_a_country_verdict() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            tokio::time::sleep(Duration::from_millis(150)).await;
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 39\r\n\r\n{\"status\":\"success\",\"countryCode\":\"US\"}")
                .await;
        });
        let result = fetch_region(
            &reqwest::Client::new(),
            &format!("http://{address}/json/"),
            Duration::from_millis(20),
        )
        .await;
        assert!(result.unwrap_err().is_timeout());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn proof_table_migrates_fresh_and_existing_databases() {
        let migrations = sqlx::migrate!();
        for existing in [false, true] {
            let pool = SqlitePoolOptions::new()
                .max_connections(1)
                .connect("sqlite::memory:")
                .await
                .unwrap();
            if existing {
                let previous = Migrator {
                    migrations: std::borrow::Cow::Owned(
                        migrations
                            .iter()
                            .filter(|migration| {
                                migration.version < 20261002120000
                            })
                            .cloned()
                            .collect(),
                    ),
                    ..Migrator::DEFAULT
                };
                previous.run(&pool).await.unwrap();
            }
            migrations.run(&pool).await.unwrap();
            sqlx::query("INSERT INTO official_login_proof (id, verified, version, mac) VALUES (0, 1, 1, 'test')")
                .execute(&pool)
                .await
                .unwrap();
            let foreign_key_errors: Vec<(String, i64, String, i64)> =
                sqlx::query_as("PRAGMA foreign_key_check")
                    .fetch_all(&pool)
                    .await
                    .unwrap();
            assert!(foreign_key_errors.is_empty());
        }
    }
}
