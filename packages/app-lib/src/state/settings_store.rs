//! Structured text store for the settings Axolotl keeps.
//!
//! A document is authoritative for the keys it carries, so it only starts
//! shadowing the database once a write has created it: an absent document
//! leaves `Settings::get` reading the row exactly as before.
//!
//! Keys are only listed here once every reader goes through `Settings::get`,
//! which is the only place that applies the stored values.

use serde::{Deserialize, Serialize};
use serde_json::Value;
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

async fn overlay_from(
    path: &Path,
    keys: &[&str],
    settings: Settings,
) -> Settings {
    let Ok(contents) = tokio::fs::read(path).await else {
        return settings;
    };
    let Ok(document) = serde_json::from_slice::<Document>(&contents) else {
        tracing::warn!(
            path = %path.display(),
            "Ignoring a settings document that cannot be read"
        );
        return settings;
    };
    if document.schema_version > SCHEMA_VERSION {
        tracing::warn!(
            path = %path.display(),
            version = document.schema_version,
            "Ignoring a settings document written by a newer build"
        );
        return settings;
    }

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
    let normalized = settings.normalized();
    let mut value = serde_json::to_value(&normalized)?;
    let Some(fields) = value.as_object_mut() else {
        return Ok(());
    };
    fields.retain(|key, _| keys.contains(&key.as_str()));

    let document = Document {
        schema_version: SCHEMA_VERSION,
        written_by: env!("CARGO_PKG_VERSION").to_string(),
        data: value,
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
