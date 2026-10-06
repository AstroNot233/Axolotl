//! Structured text store for the settings Axolotl keeps.
//!
//! A document is authoritative for the keys it carries, so it only starts
//! shadowing the database once a write has created it: an absent document
//! leaves `Settings::get` reading the row exactly as before.
//!
//! `THESEUS_SETTINGS_CONFIG_DIR` points at a read-only directory of documents a
//! deployment ships, which are read before the local ones and the row; only the
//! local directory is ever written.
//!
//! Keys are only listed here once every reader goes through `Settings::get`,
//! which is the only place that applies the stored values.
//!
//! Bump `SCHEMA_VERSION` whenever a domain's key set changes, including when
//! a key is dropped: that is what stops a build which still knows the removed
//! key from writing the document again. It versions the shape of a document,
//! unlike the `version` the `state` domain carries, which moves values.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
#[cfg(test)]
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::state::{DirectoryInfo, Settings};

const DIR_NAME: &str = "settings";
const SCHEMA_VERSION: u32 = 1;

/// The documents the store owns, with the keys each one carries.
const DOMAINS: &[(&str, &[&str])] = &[
    (
        "appearance",
        &[
            "accent_color",
            "advanced_rendering",
            "auto_hide_downloads_button",
            "close_behavior",
            "collapsed_navigation",
            "custom_background_blur",
            "custom_background_component_opacity",
            "custom_background_opacity",
            "custom_background_path",
            "custom_window_title_enabled",
            "default_page",
            "default_window_title",
            "developer_mode",
            "feature_flags",
            "hidden_nav_items",
            "hide_nametag_skins_page",
            "home_layout",
            "home_widget_background_opacity",
            "home_widgets",
            "locale",
            "log_level",
            "minimal_home_instance_id",
            "mono_font",
            "native_decorations",
            "show_files_tab_in_instances",
            "show_screenshots_tab_in_instances",
            "show_skin_selector_in_sidebar",
            "show_worlds_tab_in_instances",
            "sidebar_instance_count",
            "sync_features_across_devices",
            "theme",
            "toggle_sidebar",
            "transparent_background",
            "transparent_background_blur",
            "transparent_background_opacity",
            "ui_font",
        ],
    ),
    (
        "download",
        &[
            "auto_concurrent_downloads",
            "bypass_curseforge_download_restrictions",
            "curseforge_source",
            "download_engine",
            "max_concurrent_downloads",
            "max_concurrent_writes",
            "minecraft_file_source",
            "minecraft_metadata_source",
            "modrinth_source",
            "mojang_auth_source",
        ],
    ),
    (
        "game",
        &[
            "auto_set_java_high_performance_mode",
            "custom_env_vars",
            "enter_lightweight_mode_on_game_launch",
            "extra_launch_args",
            "force_fullscreen",
            "game_resolution",
            "hide_on_process_start",
            "hooks",
            "maximize_window",
            "memory",
        ],
    ),
    ("backup", &["backup_repository_path"]),
    ("bootstrap", &["custom_dir", "prev_custom_dir"]),
    (
        "network",
        &[
            "allow_external_scheme",
            "allow_privileged_scheme",
            "ignore_ssl_errors",
            "terracotta_public_nodes",
        ],
    ),
    (
        "privacy",
        &["discord_rpc", "telemetry", "telemetry_consent_version"],
    ),
    (
        "state",
        &[
            "migrated",
            "onboarded",
            "onboarding_instance_tour_completed",
            "onboarding_version",
            "pending_update_toast_for_version",
            "version",
        ],
    ),
];

#[derive(Deserialize, Serialize)]
struct Document {
    schema_version: u32,
    written_by: String,
    data: Value,
}

enum Stored {
    Missing,
    Unreadable,
    Newer(Document),
    Ready(Document),
}

/// The settings directory, resolved before `State` exists.
static SETTINGS_DIR: OnceLock<PathBuf> = OnceLock::new();

/// The read-only directory of documents a deployment ships, when it has one.
static CONFIG_DIR: OnceLock<PathBuf> = OnceLock::new();

const CONFIG_DIR_ENV: &str = "THESEUS_SETTINGS_CONFIG_DIR";

#[cfg(test)]
thread_local! {
    static TEST_DIR: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

pub(crate) fn init(app_identifier: &str) {
    let Some(settings_dir) =
        DirectoryInfo::initial_settings_dir_path(app_identifier)
    else {
        return;
    };
    let _ = SETTINGS_DIR.set(settings_dir);
    if let Some(config_dir) = std::env::var_os(CONFIG_DIR_ENV)
        && !config_dir.is_empty()
    {
        let _ = CONFIG_DIR.set(PathBuf::from(config_dir));
    }
}

/// Where the documents live: the directory startup resolved, or one of this
/// thread's own while running tests.
fn settings_root() -> Option<PathBuf> {
    #[cfg(test)]
    return TEST_DIR.with(|dir| {
        let mut dir = dir.borrow_mut();
        let path =
            dir.get_or_insert_with(|| tempfile::tempdir().unwrap().keep());
        Some(path.clone())
    });

    #[cfg(not(test))]
    SETTINGS_DIR.get().cloned()
}

/// Whether the store has a directory to write to, which startup gives it.
pub(crate) fn is_active() -> bool {
    settings_root().is_some()
}

/// Whether the row still has to hand its settings over to the documents, which
/// is what an installation that predates them does once: `database_existed`
/// keeps a fresh installation out, and the directory keeps one that already
/// took the row over from doing it again.
pub(crate) async fn needs_seeding(database_existed: bool) -> bool {
    needs_seeding_at(SETTINGS_DIR.get().map(PathBuf::as_path), database_existed)
        .await
}

async fn needs_seeding_at(
    settings_dir: Option<&Path>,
    database_existed: bool,
) -> bool {
    let Some(settings_dir) = settings_dir else {
        return false;
    };
    database_existed
        && !tokio::fs::try_exists(settings_dir.join(DIR_NAME))
            .await
            .unwrap_or(false)
}

fn local_path(name: &str) -> Option<PathBuf> {
    Some(settings_root()?.join(DIR_NAME).join(format!("{name}.json")))
}

fn config_path(name: &str) -> Option<PathBuf> {
    Some(CONFIG_DIR.get()?.join(format!("{name}.json")))
}

/// Applies the stored keys on top of `settings`, keeping the database value
/// for every key no document carries.
pub(crate) async fn overlay(settings: Settings) -> Settings {
    let mut settings = settings;
    for (name, keys) in DOMAINS {
        settings = overlay_layers(
            config_path(name).as_deref(),
            local_path(name).as_deref(),
            keys,
            settings,
        )
        .await;
    }
    settings
}

/// Layers the documents of one domain, so a default gives way to the local file
/// and to the row.
async fn overlay_layers(
    defaults: Option<&Path>,
    local: Option<&Path>,
    keys: &[&str],
    settings: Settings,
) -> Settings {
    let mut settings = settings;
    for path in [defaults, local].into_iter().flatten() {
        settings = overlay_from(path, keys, settings).await;
    }
    settings
}

/// Persists every domain, without letting a failure reach the caller: the row
/// keeps the values a document has not taken over yet.
pub(crate) async fn store(settings: &Settings) {
    for (name, keys) in DOMAINS {
        let Some(path) = local_path(name) else {
            return;
        };
        if let Err(error) = store_to(&path, keys, settings).await {
            tracing::warn!(
                path = %path.display(),
                %error,
                "Failed to save the {name} settings"
            );
        }
    }
}

/// Drops the keys no domain knows, for documents another version left behind.
/// Documents written by a newer build are left alone.
pub async fn sanitise() -> crate::Result<usize> {
    let mut removed = 0;
    for (name, keys) in DOMAINS {
        let Some(path) = local_path(name) else {
            break;
        };
        removed += sanitise_at(&path, keys).await?;
    }
    Ok(removed)
}

async fn sanitise_at(path: &Path, keys: &[&str]) -> crate::Result<usize> {
    let Stored::Ready(mut document) = read(path).await else {
        return Ok(0);
    };
    let Some(data) = document.data.as_object_mut() else {
        return Ok(0);
    };
    let before = data.len();
    data.retain(|key, _| keys.contains(&key.as_str()));
    let removed = before - data.len();
    if removed == 0 {
        return Ok(0);
    }

    crate::util::io::write(path, serde_json::to_vec_pretty(&document)?).await?;
    tracing::info!(
        path = %path.display(),
        removed,
        "Removed settings keys no domain knows"
    );
    Ok(removed)
}

/// The stored entries of one document, for callers that read a few keys
/// without going through `Settings`. Empty when there is nothing to read.
pub(crate) async fn stored(domain: &str) -> Map<String, Value> {
    stored_layers(
        config_path(domain).as_deref(),
        local_path(domain).as_deref(),
    )
    .await
}

/// Layers the entries of one domain, so a default gives way to the local file.
async fn stored_layers(
    defaults: Option<&Path>,
    local: Option<&Path>,
) -> Map<String, Value> {
    let mut entries = Map::new();
    for path in [defaults, local].into_iter().flatten() {
        entries.extend(stored_at(path).await);
    }
    entries
}

async fn stored_at(path: &Path) -> Map<String, Value> {
    match read(path).await {
        Stored::Ready(document) => {
            document.data.as_object().cloned().unwrap_or_default()
        }
        _ => Map::new(),
    }
}

async fn read(path: &Path) -> Stored {
    let Ok(contents) = tokio::fs::read(path).await else {
        return Stored::Missing;
    };
    let Ok(document) = serde_json::from_slice::<Document>(&contents) else {
        return Stored::Unreadable;
    };
    if document.schema_version > SCHEMA_VERSION {
        Stored::Newer(document)
    } else {
        Stored::Ready(document)
    }
}

async fn overlay_from(
    path: &Path,
    keys: &[&str],
    settings: Settings,
) -> Settings {
    let document = match read(path).await {
        Stored::Ready(document) => document,
        Stored::Missing => return settings,
        Stored::Unreadable => {
            tracing::warn!(
                path = %path.display(),
                "Ignoring a settings document that cannot be read"
            );
            return settings;
        }
        Stored::Newer(document) => {
            tracing::warn!(
                path = %path.display(),
                version = document.schema_version,
                "Ignoring a settings document written by a newer build"
            );
            return settings;
        }
    };

    let (Ok(mut value), Some(stored)) =
        (serde_json::to_value(&settings), document.data.as_object())
    else {
        return settings;
    };
    let Some(target) = value.as_object_mut() else {
        return settings;
    };
    for (key, stored) in stored {
        if keys.contains(&key.as_str()) {
            target.insert(key.clone(), stored.clone());
        }
    }
    serde_json::from_value(value).unwrap_or(settings)
}

async fn store_to(
    path: &Path,
    keys: &[&str],
    settings: &Settings,
) -> crate::Result<()> {
    let Some(mut data) = base_data(path).await else {
        return Ok(());
    };

    let normalized = settings.normalized();
    let mut value = serde_json::to_value(&normalized)?;
    let Some(fields) = value.as_object_mut() else {
        return Ok(());
    };
    fields.retain(|key, _| keys.contains(&key.as_str()));
    data.extend(fields.iter().map(|(k, v)| (k.clone(), v.clone())));

    write_document(path, data).await
}

/// Writes one key of the domain that lists it, leaving the rest of the
/// document as it is. Keys no domain lists are ignored.
pub(crate) async fn store_key(key: &str, value: Value) {
    let Some(path) = key_path(key) else {
        return;
    };
    if let Err(error) = set_key_at(&path, key, value).await {
        tracing::warn!(
            path = %path.display(),
            %error,
            "Failed to save a setting"
        );
    }
}

/// Writes entries of a document the domain table does not derive, which is how
/// a domain that owns its keys is stored. Such a document is left out of
/// `sanitise`, since no domain lists its keys.
pub(crate) async fn store_in(domain: &str, entries: &[(&str, Value)]) {
    let Some(path) = local_path(domain) else {
        return;
    };
    if let Err(error) = set_entries_at(&path, entries).await {
        tracing::warn!(
            path = %path.display(),
            %error,
            "Failed to save the {domain} settings"
        );
    }
}

/// Clears `key` while the stored document still holds `value`, which keeps the
/// settings from referring to something that no longer exists.
pub(crate) async fn clear_key_if(key: &str, value: &str) {
    let Some(path) = key_path(key) else {
        return;
    };
    if let Err(error) = clear_key_if_at(&path, key, value).await {
        tracing::warn!(
            path = %path.display(),
            %error,
            "Failed to clear a setting"
        );
    }
}

async fn clear_key_if_at(
    path: &Path,
    key: &str,
    value: &str,
) -> crate::Result<()> {
    if stored_at(path).await.get(key).and_then(Value::as_str) != Some(value) {
        return Ok(());
    }
    set_key_at(path, key, Value::Null).await
}

/// The document that lists `key`, for the writers that own a single key.
fn key_path(key: &str) -> Option<PathBuf> {
    let Some((name, _)) = DOMAINS.iter().find(|(_, keys)| keys.contains(&key))
    else {
        tracing::debug!(key, "Ignoring a settings key no domain lists");
        return None;
    };
    local_path(name)
}

async fn set_key_at(path: &Path, key: &str, value: Value) -> crate::Result<()> {
    let Some(mut data) = base_data(path).await else {
        return Ok(());
    };
    data.insert(key.to_string(), value);
    write_document(path, data).await
}

async fn set_entries_at(
    path: &Path,
    entries: &[(&str, Value)],
) -> crate::Result<()> {
    let Some(mut data) = base_data(path).await else {
        return Ok(());
    };
    data.extend(
        entries
            .iter()
            .map(|(key, value)| (key.to_string(), value.clone())),
    );
    write_document(path, data).await
}

/// The stored entries as a base, so a document another version wrote keeps the
/// entries this build does not know. `None` means the document must not be
/// written, which keeps one from a newer build read-only and leaves one that
/// cannot be read for its owner to sort out.
async fn base_data(path: &Path) -> Option<Map<String, Value>> {
    match read(path).await {
        Stored::Ready(document) => document.data.as_object().cloned(),
        Stored::Newer(document) => {
            tracing::warn!(
                path = %path.display(),
                version = document.schema_version,
                "Leaving a settings document written by a newer build alone"
            );
            None
        }
        Stored::Unreadable => {
            tracing::warn!(
                path = %path.display(),
                "Leaving a settings document that cannot be read alone"
            );
            None
        }
        Stored::Missing => Some(Map::new()),
    }
}

async fn write_document(
    path: &Path,
    data: Map<String, Value>,
) -> crate::Result<()> {
    let document = Document {
        schema_version: SCHEMA_VERSION,
        written_by: env!("CARGO_PKG_VERSION").to_string(),
        data: Value::Object(data),
    };
    if let Some(parent) = path.parent() {
        crate::util::io::create_dir_all(parent).await?;
    }
    crate::util::io::write(path, serde_json::to_vec_pretty(&document)?).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn fresh_settings() -> Settings {
        let pool = crate::state::db::migrated_test_pool().await;
        Settings::get(&pool).await.unwrap()
    }

    fn appearance() -> &'static [&'static str] {
        DOMAINS[0].1
    }

    fn document(data: &str) -> String {
        format!(
            r#"{{"schema_version":{SCHEMA_VERSION},"written_by":"9.9.9","data":{data}}}"#
        )
    }

    #[tokio::test]
    async fn every_stored_key_is_a_settings_field() {
        let value = serde_json::to_value(fresh_settings().await).unwrap();
        let fields = value.as_object().unwrap();

        for (name, keys) in DOMAINS {
            for key in *keys {
                assert!(fields.contains_key(*key), "{name} lists {key}");
            }
        }
    }

    #[tokio::test]
    async fn stored_keys_shadow_the_row_after_a_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("appearance.json");

        let mut settings = fresh_settings().await;
        settings.accent_color = crate::state::AccentColor::Pink;
        store_to(&path, appearance(), &settings).await.unwrap();

        let mut stored = fresh_settings().await;
        stored.accent_color = crate::state::AccentColor::Blue;
        let merged = overlay_from(&path, appearance(), stored).await;
        assert_eq!(merged.accent_color, crate::state::AccentColor::Pink);
    }

    #[tokio::test]
    async fn a_fresh_installation_has_nothing_to_hand_over() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!needs_seeding_at(Some(dir.path()), false).await);
    }

    #[tokio::test]
    async fn an_installation_without_documents_takes_the_row_over() {
        let dir = tempfile::tempdir().unwrap();
        assert!(needs_seeding_at(Some(dir.path()), true).await);

        std::fs::create_dir_all(dir.path().join(DIR_NAME)).unwrap();
        assert!(!needs_seeding_at(Some(dir.path()), true).await);
    }

    #[tokio::test]
    async fn a_store_without_a_directory_has_nothing_to_write_to() {
        assert!(!needs_seeding_at(None, true).await);
    }

    #[tokio::test]
    async fn a_default_document_gives_way_to_the_local_one() {
        let dir = tempfile::tempdir().unwrap();
        let defaults = dir.path().join("defaults.json");
        let local = dir.path().join("local.json");
        std::fs::write(
            &defaults,
            document(r#"{"locale":"de-DE","theme":"oled"}"#),
        )
        .unwrap();
        std::fs::write(&local, document(r#"{"locale":"fr-FR"}"#)).unwrap();

        let mut stored = fresh_settings().await;
        stored.locale = "en-US".to_string();
        stored.theme = crate::state::Theme::Dark;
        let merged =
            overlay_layers(Some(&defaults), Some(&local), appearance(), stored)
                .await;

        assert_eq!(merged.locale, "fr-FR");
        assert_eq!(merged.theme.as_str(), "oled");
    }

    #[tokio::test]
    async fn a_default_document_applies_without_a_local_one() {
        let dir = tempfile::tempdir().unwrap();
        let defaults = dir.path().join("defaults.json");
        std::fs::write(&defaults, document(r#"{"locale":"de-DE"}"#)).unwrap();

        let mut stored = fresh_settings().await;
        stored.locale = "en-US".to_string();
        let merged =
            overlay_layers(Some(&defaults), None, appearance(), stored).await;
        assert_eq!(merged.locale, "de-DE");
    }

    #[tokio::test]
    async fn stored_entries_layer_a_default_under_the_local_file() {
        let dir = tempfile::tempdir().unwrap();
        let defaults = dir.path().join("defaults.json");
        let local = dir.path().join("local.json");
        std::fs::write(
            &defaults,
            document(r#"{"proxy_mode":"system","proxy_url":"http://default"}"#),
        )
        .unwrap();
        std::fs::write(&local, document(r#"{"proxy_mode":"custom"}"#)).unwrap();

        let entries = stored_layers(Some(&defaults), Some(&local)).await;
        assert_eq!(entries.get("proxy_mode"), Some(&Value::from("custom")));
        assert_eq!(
            entries.get("proxy_url"),
            Some(&Value::from("http://default"))
        );
    }

    #[tokio::test]
    async fn keys_no_document_carries_stay_with_the_row() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("appearance.json");

        let mut settings = fresh_settings().await;
        settings.custom_dir = Some("/tmp/kept".to_string());
        store_to(&path, appearance(), &settings).await.unwrap();
        assert!(
            !std::fs::read_to_string(&path)
                .unwrap()
                .contains("custom_dir")
        );

        let mut stored = fresh_settings().await;
        stored.custom_dir = Some("/tmp/row".to_string());
        let merged = overlay_from(&path, appearance(), stored).await;
        assert_eq!(merged.custom_dir.as_deref(), Some("/tmp/row"));
    }

    #[tokio::test]
    async fn a_write_keeps_the_keys_this_build_does_not_know() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("appearance.json");
        std::fs::write(
            &path,
            document(r#"{"theme":"oled","from_a_newer_build":7}"#),
        )
        .unwrap();

        store_to(&path, appearance(), &fresh_settings().await)
            .await
            .unwrap();

        let stored: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap())
                .unwrap();
        assert_eq!(stored["data"]["from_a_newer_build"], 7);
        assert!(stored["data"]["accent_color"].is_string());
        assert_eq!(stored["written_by"], env!("CARGO_PKG_VERSION"));
    }

    #[tokio::test]
    async fn a_document_from_a_newer_build_is_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("appearance.json");
        let newer = format!(
            r#"{{"schema_version":{},"written_by":"9.9.9","data":{{"theme":"oled"}}}}"#,
            SCHEMA_VERSION + 1
        );
        std::fs::write(&path, &newer).unwrap();

        store_to(&path, appearance(), &fresh_settings().await)
            .await
            .unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), newer);
    }

    #[tokio::test]
    async fn sanitise_drops_the_keys_no_domain_knows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("appearance.json");
        std::fs::write(
            &path,
            document(r#"{"theme":"oled","from_a_newer_build":7}"#),
        )
        .unwrap();

        assert_eq!(sanitise_at(&path, appearance()).await.unwrap(), 1);

        let stored: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap())
                .unwrap();
        assert_eq!(stored["data"]["theme"], "oled");
        assert!(stored["data"].get("from_a_newer_build").is_none());
    }

    #[tokio::test]
    async fn sanitise_leaves_a_newer_document_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("appearance.json");
        let newer = format!(
            r#"{{"schema_version":{},"written_by":"9.9.9","data":{{"from_a_newer_build":7}}}}"#,
            SCHEMA_VERSION + 1
        );
        std::fs::write(&path, &newer).unwrap();

        assert_eq!(sanitise_at(&path, appearance()).await.unwrap(), 0);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), newer);
    }

    #[tokio::test]
    async fn a_single_key_write_keeps_the_rest_of_the_document() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("game.json");
        std::fs::write(
            &path,
            document(r#"{"theme":"oled","from_a_newer_build":7}"#),
        )
        .unwrap();

        set_key_at(&path, "force_fullscreen", Value::Bool(true))
            .await
            .unwrap();

        let stored: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap())
                .unwrap();
        assert_eq!(stored["data"]["force_fullscreen"], true);
        assert_eq!(stored["data"]["theme"], "oled");
        assert_eq!(stored["data"]["from_a_newer_build"], 7);
    }

    #[tokio::test]
    async fn entries_a_domain_owns_are_stored_alongside_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("proxy.json");
        std::fs::write(&path, document(r#"{"proxy_mode":"system"}"#)).unwrap();

        set_entries_at(
            &path,
            &[
                ("proxy_mode", Value::from("custom")),
                ("proxy_url", Value::from("http://localhost:8080")),
            ],
        )
        .await
        .unwrap();

        let stored = stored_at(&path).await;
        assert_eq!(stored.get("proxy_mode"), Some(&Value::from("custom")));
        assert_eq!(
            stored.get("proxy_url"),
            Some(&Value::from("http://localhost:8080"))
        );
    }

    #[tokio::test]
    async fn stored_reads_the_entries_of_a_document() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("privacy.json");
        std::fs::write(&path, document(r#"{"telemetry":true,"other":1}"#))
            .unwrap();

        let stored = stored_at(&path).await;
        assert_eq!(stored.get("telemetry"), Some(&Value::Bool(true)));
        assert_eq!(stored.get("other"), Some(&Value::from(1)));
    }

    #[tokio::test]
    async fn stored_is_empty_when_there_is_nothing_to_read() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("privacy.json");
        assert!(stored_at(&missing).await.is_empty());

        let corrupt = dir.path().join("corrupt.json");
        std::fs::write(&corrupt, b"{ not json").unwrap();
        assert!(stored_at(&corrupt).await.is_empty());

        let newer = dir.path().join("newer.json");
        std::fs::write(
            &newer,
            format!(
                r#"{{"schema_version":{},"written_by":"9.9.9","data":{{"telemetry":true}}}}"#,
                SCHEMA_VERSION + 1
            ),
        )
        .unwrap();
        assert!(stored_at(&newer).await.is_empty());
    }

    #[tokio::test]
    async fn a_reference_to_something_removed_is_cleared() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("appearance.json");
        std::fs::write(
            &path,
            document(r#"{"minimal_home_instance_id":"gone","other":1}"#),
        )
        .unwrap();

        clear_key_if_at(&path, "minimal_home_instance_id", "gone")
            .await
            .unwrap();

        let stored = stored_at(&path).await;
        assert_eq!(stored.get("minimal_home_instance_id"), Some(&Value::Null));
        assert_eq!(stored.get("other"), Some(&Value::from(1)));
    }

    #[tokio::test]
    async fn a_reference_to_something_else_stays() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("appearance.json");
        std::fs::write(
            &path,
            document(r#"{"minimal_home_instance_id":"kept"}"#),
        )
        .unwrap();

        clear_key_if_at(&path, "minimal_home_instance_id", "gone")
            .await
            .unwrap();
        assert_eq!(
            stored_at(&path).await.get("minimal_home_instance_id"),
            Some(&Value::from("kept"))
        );
    }

    #[tokio::test]
    async fn a_missing_document_leaves_the_row_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("appearance.json");

        let mut stored = fresh_settings().await;
        stored.locale = "de-DE".to_string();
        let merged = overlay_from(&path, appearance(), stored).await;
        assert_eq!(merged.locale, "de-DE");
    }

    #[tokio::test]
    async fn a_document_from_a_newer_build_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("appearance.json");
        std::fs::write(
            &path,
            format!(
                r#"{{"schema_version":{},"written_by":"9.9.9","data":{{"locale":"de-DE"}}}}"#,
                SCHEMA_VERSION + 1
            ),
        )
        .unwrap();

        let mut stored = fresh_settings().await;
        stored.locale = "fr-FR".to_string();
        let merged = overlay_from(&path, appearance(), stored).await;
        assert_eq!(merged.locale, "fr-FR");
    }

    #[tokio::test]
    async fn a_corrupt_document_is_left_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("appearance.json");
        std::fs::write(&path, b"{ not json").unwrap();

        let mut stored = fresh_settings().await;
        stored.locale = "fr-FR".to_string();
        let merged = overlay_from(&path, appearance(), stored).await;
        assert_eq!(merged.locale, "fr-FR");
        assert!(path.exists());
    }

    #[tokio::test]
    async fn a_corrupt_document_is_neither_written_nor_cleaned() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("appearance.json");
        std::fs::write(&path, b"{ not json").unwrap();

        store_to(&path, appearance(), &fresh_settings().await)
            .await
            .unwrap();
        assert_eq!(sanitise_at(&path, appearance()).await.unwrap(), 0);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ not json");
    }
}
