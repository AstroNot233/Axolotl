//! The launcher's own settings live in eight documents rather than one: the
//! keys of `Settings` are split by `DOMAINS`, so `appearance.json` carries the
//! look of the launcher while `download.json` carries how it fetches.
//!
//! A read layers, bottom up:
//!
//! 1. the build defaults, which every key starts at;
//! 2. the settings row, for the domains whose document is missing or cannot be
//!    read;
//! 3. the deployment's own documents, which a build ships and never writes
//!    (not read yet);
//! 4. the local documents, the only layer ever written.
//!
//! A domain that has a document is answered by that document alone: the row
//! does not contribute to it, or a key the document leaves out would come back
//! from a value the row froze. The row is only asked for a domain with no
//! document at all, so one damaged file costs that domain and nothing else.
//!
//! Two layers are not in place yet. `Settings` has no `Default` — its defaults
//! are the migration's column defaults — so the row stands in for the first
//! layer and for the second at once: a key a document leaves out reads the row
//! today, which is the one place this read promises less than the layering
//! above. The deployment's documents are not read at all.
//!
//! The domains are written one at a time, so a launcher stopped between two of
//! them leaves the domains a generation apart. They carry unrelated settings,
//! which is why that is accepted rather than paid for with a cross-file write.
//!
//! `personalized_ads` is in no document: it is the one `Settings` field marked
//! `#[serde(skip)]`, it stays a column of the row alone, and listing it here
//! would promise a document that never carries it.
//!
//! Nothing reads these documents yet: `Settings::get` still answers from the
//! row, and nothing writes them either. Both arrive with the wiring that
//! follows.

use serde_json::{Map, Value};

use super::{Documents, Stored};
use crate::state::settings::Settings;
use crate::state::store::Handle;

/// The domains with the keys their document carries. A key belongs to exactly
/// one of them, and a key none of them lists is not stored in a document at
/// all.
///
/// The version travels with the domain and is bumped when that domain's key set
/// changes, including when a key is dropped: it versions the shape of a
/// document, unlike the `version` the `state` domain carries as a key, which
/// versions the values it holds.
const DOMAINS: &[(&str, &[&str], u32)] = &[
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
            "nametag_uses_app_font",
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
        1,
    ),
    (
        "download",
        &[
            "auto_concurrent_downloads",
            "bypass_curseforge_download_restrictions",
            "curseforge_source",
            "doh_enabled",
            "max_concurrent_downloads",
            "max_concurrent_writes",
            "minecraft_file_source",
            "minecraft_metadata_source",
            "modrinth_source",
            "mojang_auth_source",
            "resource_chinese_filenames",
            "resource_preserve_filenames",
        ],
        1,
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
        1,
    ),
    ("backup", &["backup_repository_path"], 1),
    ("bootstrap", &["custom_dir", "prev_custom_dir"], 1),
    (
        "network",
        &[
            "allow_external_scheme",
            "allow_privileged_scheme",
            "ignore_ssl_errors",
            "terracotta_public_nodes",
        ],
        1,
    ),
    (
        "privacy",
        &["discord_rpc", "telemetry", "telemetry_consent_version"],
        1,
    ),
    (
        "state",
        &[
            "auto_download_updates",
            "migrated",
            "onboarded",
            "onboarding_instance_tour_completed",
            "onboarding_version",
            "pending_update_toast_for_version",
            "skipped_update",
            "version",
        ],
        1,
    ),
];

/// One document as it stands on disk. Its keys are not interpreted yet, so a
/// value this build cannot read costs only that key.
type Document = Map<String, Value>;

/// The settings the documents and the row together describe.
pub(crate) async fn read(
    documents: &Documents<'_>,
    handle: &Handle,
) -> crate::Result<Settings> {
    // Layers 1 and 2 are the build defaults and the row. `Settings` has no
    // `Default` yet — its defaults live in the migration's column defaults — so
    // the row stands in for both: a domain that keeps it reads what the row
    // says, and so does a key a document leaves out. The row is read once and
    // projected by domain below, not once per domain.
    let mut values = row_values(handle).await?;

    for &(name, keys, schema_version) in DOMAINS {
        let stored = documents.stored::<Document>(name, schema_version).await;
        // Layer 3, the deployment's documents, would come first and is not read
        // yet; it would contribute keys for every domain, not just the ones
        // without a local document.
        match stored {
            // Layer 4: a document is the authority for the domain it exists
            // for, whichever build wrote it.
            Stored::Ready { document, .. } | Stored::Newer { document, .. } => {
                overlay(&mut values, keys, &document, name);
            }
            // Layer 2 reaches the result here: a domain with no document is the
            // only kind the row answers, and `values` still holds its values.
            Stored::Missing | Stored::Unreadable => {}
        }
    }

    Ok(serde_json::from_value(Value::Object(values))?)
}

/// The settings as the row holds them, keyed the way a document keys them.
async fn row_values(handle: &Handle) -> crate::Result<Map<String, Value>> {
    let settings = Settings::get(handle).await?;
    Ok(serde_json::from_value(serde_json::to_value(&settings)?)?)
}

/// Lays one layer over `values`, for the keys `keys` lists only: a key outside
/// that list is ignored, and so is a value the type cannot take, which costs
/// that key and leaves what the layer below gave it.
fn overlay(
    values: &mut Map<String, Value>,
    keys: &[&str],
    source: &Document,
    name: &str,
) {
    for &key in keys {
        let Some(value) = source.get(key) else {
            continue;
        };
        let kept = values.insert(key.to_string(), value.clone());
        let unreadable =
            serde_json::from_value::<Settings>(Value::Object(values.clone()))
                .err();
        if let Some(error) = unreadable {
            if let Some(kept) = kept {
                values.insert(key.to_string(), kept);
            } else {
                values.remove(key);
            }
            tracing::warn!(
                document = name,
                key,
                %error,
                "Ignoring a settings value that cannot be read"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use sqlx::SqlitePool;
    use sqlx::sqlite::SqlitePoolOptions;

    use super::*;

    /// An installation the migrations have set up, whose row is the only whole
    /// `Settings` this build can put together.
    async fn migrated_pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();
        pool
    }

    fn document_keys() -> BTreeSet<&'static str> {
        DOMAINS
            .iter()
            .flat_map(|(_, keys, _)| keys.iter().copied())
            .collect()
    }

    #[tokio::test]
    async fn the_domains_carry_every_key_the_settings_serialize() {
        let pool = migrated_pool().await;
        let settings = Settings::get(&Handle::new(pool)).await.unwrap();
        let serialized: BTreeSet<String> = match serde_json::to_value(&settings)
            .unwrap()
        {
            Value::Object(map) => map.into_iter().map(|(key, _)| key).collect(),
            other => panic!("the settings serialize as {other:?}"),
        };
        let documents = document_keys();

        let unlisted: Vec<&String> = serialized
            .iter()
            .filter(|key| !documents.contains(key.as_str()))
            .collect();
        assert!(unlisted.is_empty(), "keys no domain carries: {unlisted:?}");

        let unknown: Vec<&str> = documents
            .iter()
            .filter(|key| !serialized.contains(**key))
            .copied()
            .collect();
        assert!(
            unknown.is_empty(),
            "keys the settings do not serialize: {unknown:?}"
        );
    }

    #[test]
    fn no_key_belongs_to_two_domains() {
        let mut carried = BTreeSet::new();
        let mut repeated = Vec::new();
        for (name, keys, _) in DOMAINS {
            for key in *keys {
                if !carried.insert(*key) {
                    repeated.push(format!("{key} of {name}"));
                }
            }
        }
        assert!(repeated.is_empty(), "keys two domains carry: {repeated:?}");
    }

    #[tokio::test]
    async fn a_stored_key_is_read_and_the_rest_stay_with_the_row() {
        let pool = migrated_pool().await;
        let row = Settings::get(&Handle::new(pool.clone())).await.unwrap();
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("backup.json"),
            r#"{"schema_version":1,"data":{"backup_repository_path":"/backups","nonsense":1}}"#,
        )
        .unwrap();
        std::fs::write(
            root.path().join("appearance.json"),
            r#"{"schema_version":1,"data":{"custom_background_blur":"lots"}}"#,
        )
        .unwrap();

        let documents = Documents::at(root.path().to_path_buf(), &pool);
        let settings =
            read(&documents, &Handle::new(pool.clone())).await.unwrap();

        assert_eq!(
            settings.backup_repository_path.as_deref(),
            Some("/backups"),
            "a key the document carries is read from it"
        );
        assert_eq!(
            settings.custom_background_blur, row.custom_background_blur,
            "a value the type cannot take costs its own key only"
        );
        assert_eq!(
            settings.locale, row.locale,
            "a domain whose document leaves a key out reads the row for it"
        );
    }
}
