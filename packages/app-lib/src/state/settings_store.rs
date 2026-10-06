//! Structured text store for the settings Axolotl keeps.
//!
//! A document is authoritative for the keys it carries, so it only starts
//! shadowing the database once a write has created it: an absent document
//! leaves `Settings::get` reading the row exactly as before.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::state::{DirectoryInfo, Settings};

const DIR_NAME: &str = "settings";
const SCHEMA_VERSION: u32 = 1;

/// Keys of `appearance.json`, the first domain that moved out of the row.
const APPEARANCE_KEYS: &[&str] = &[
    "accent_color",
    "auto_hide_downloads_button",
    "close_behavior",
    "custom_background_blur",
    "custom_background_component_opacity",
    "custom_background_opacity",
    "custom_background_path",
    "feature_flags",
    "hidden_nav_items",
    "hide_nametag_skins_page",
    "home_layout",
    "home_widget_background_opacity",
    "home_widgets",
    "mono_font",
    "show_files_tab_in_instances",
    "show_screenshots_tab_in_instances",
    "show_skin_selector_in_sidebar",
    "show_worlds_tab_in_instances",
    "sidebar_instance_count",
    "sync_features_across_devices",
    "toggle_sidebar",
    "transparent_background",
    "transparent_background_blur",
    "transparent_background_opacity",
    "ui_font",
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

fn appearance_path() -> Option<PathBuf> {
    Some(SETTINGS_DIR.get()?.join(DIR_NAME).join("appearance.json"))
}

/// Applies the stored keys on top of `settings`, keeping the database value
/// for every key the document does not carry.
pub(crate) async fn overlay(settings: Settings) -> Settings {
    match appearance_path() {
        Some(path) => overlay_from(&path, settings).await,
        None => settings,
    }
}

/// Persists the appearance keys, without letting a failure reach the caller:
/// the row is still the source of truth while the store is introduced.
pub(crate) async fn store(settings: &Settings) {
    let Some(path) = appearance_path() else {
        return;
    };
    if let Err(error) = store_to(&path, settings).await {
        tracing::warn!(
            path = %path.display(),
            %error,
            "Failed to save the appearance settings"
        );
    }
}

async fn overlay_from(path: &Path, settings: Settings) -> Settings {
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
        if APPEARANCE_KEYS.contains(&key.as_str()) {
            target.insert(key.clone(), stored.clone());
        }
    }
    serde_json::from_value(value).unwrap_or(settings)
}

async fn store_to(path: &Path, settings: &Settings) -> crate::Result<()> {
    let normalized = settings.normalized();
    let mut value = serde_json::to_value(&normalized)?;
    let Some(fields) = value.as_object_mut() else {
        return Ok(());
    };
    fields.retain(|key, _| APPEARANCE_KEYS.contains(&key.as_str()));

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

    #[tokio::test]
    async fn appearance_keys_shadow_the_row_after_a_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("appearance.json");

        let mut settings = fresh_settings().await;
        settings.accent_color = crate::state::AccentColor::Pink;
        store_to(&path, &settings).await.unwrap();

        let mut stored = fresh_settings().await;
        stored.accent_color = crate::state::AccentColor::Blue;
        let merged = overlay_from(&path, stored).await;
        assert_eq!(merged.accent_color, crate::state::AccentColor::Pink);
    }

    #[tokio::test]
    async fn keys_outside_the_domain_stay_with_the_row() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("appearance.json");

        let mut settings = fresh_settings().await;
        settings.locale = "de-DE".to_string();
        store_to(&path, &settings).await.unwrap();

        let mut stored = fresh_settings().await;
        stored.locale = "fr-FR".to_string();
        let merged = overlay_from(&path, stored).await;
        assert_eq!(merged.locale, "fr-FR");
    }

    #[tokio::test]
    async fn a_missing_document_leaves_the_row_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("appearance.json");

        let mut stored = fresh_settings().await;
        stored.locale = "de-DE".to_string();
        let merged = overlay_from(&path, stored).await;
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
        let merged = overlay_from(&path, stored).await;
        assert_eq!(merged.locale, "fr-FR");
    }

    #[tokio::test]
    async fn a_corrupt_document_is_left_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("appearance.json");
        std::fs::write(&path, b"{ not json").unwrap();

        let mut stored = fresh_settings().await;
        stored.locale = "fr-FR".to_string();
        let merged = overlay_from(&path, stored).await;
        assert_eq!(merged.locale, "fr-FR");
        assert!(path.exists());
    }
}
