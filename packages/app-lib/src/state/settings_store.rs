//! Structured text store for the settings Axolotl keeps.
//!
//! A document is authoritative for the keys it carries, so it only starts
//! shadowing the database once a write has created it: an absent document
//! leaves `Settings::get` reading the row exactly as before.
//!
//! Keys are only listed here once every reader goes through `Settings::get`,
//! which is the only place that applies the stored values.
//!
//! Bump `SCHEMA_VERSION` whenever a domain's key set changes, including when
//! a key is dropped: that is what stops a build which still knows the removed
//! key from writing the document again.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
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
            "game_resolution",
            "hide_on_process_start",
            "hooks",
            "maximize_window",
            "memory",
        ],
    ),
    (
        "network",
        &[
            "allow_external_scheme",
            "allow_privileged_scheme",
            "ignore_ssl_errors",
            "terracotta_public_nodes",
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

pub(crate) fn init(app_identifier: &str) {
    let Some(settings_dir) =
        DirectoryInfo::initial_settings_dir_path(app_identifier)
    else {
        return;
    };
    let _ = SETTINGS_DIR.set(settings_dir);
}

fn domain_path(name: &str) -> Option<PathBuf> {
    Some(
        SETTINGS_DIR
            .get()?
            .join(DIR_NAME)
            .join(format!("{name}.json")),
    )
}

/// Applies the stored keys on top of `settings`, keeping the database value
/// for every key no document carries.
pub(crate) async fn overlay(settings: Settings) -> Settings {
    let mut settings = settings;
    for (name, keys) in DOMAINS {
        let Some(path) = domain_path(name) else {
            return settings;
        };
        settings = overlay_from(&path, keys, settings).await;
    }
    settings
}

/// Persists every domain, without letting a failure reach the caller: the row
/// is still the source of truth while the store is introduced.
pub(crate) async fn store(settings: &Settings) {
    for (name, keys) in DOMAINS {
        let Some(path) = domain_path(name) else {
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
        let Some(path) = domain_path(name) else {
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
    let stored = read(path).await;
    if let Stored::Newer(document) = &stored {
        tracing::warn!(
            path = %path.display(),
            version = document.schema_version,
            "Leaving a settings document written by a newer build alone"
        );
        return Ok(());
    }

    let normalized = settings.normalized();
    let mut value = serde_json::to_value(&normalized)?;
    let Some(fields) = value.as_object_mut() else {
        return Ok(());
    };
    fields.retain(|key, _| keys.contains(&key.as_str()));

    // Keep the keys this build does not know, so a document another version
    // wrote is not stripped of them.
    let mut data = match stored {
        Stored::Ready(document) => document.data,
        _ => Value::Object(Map::new()),
    }
    .as_object()
    .cloned()
    .unwrap_or_default();
    data.extend(fields.iter().map(|(k, v)| (k.clone(), v.clone())));

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
}
