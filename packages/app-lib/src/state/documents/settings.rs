//! The launcher's own settings live in eight documents rather than one: the
//! keys of `Settings` are split by `DOMAINS`, so `appearance.json` carries the
//! look of the launcher while `download.json` carries how it fetches.
//!
//! A read layers, bottom up:
//!
//! 1. the build defaults, which every key starts at;
//! 2. the settings row, for the domains whose document is missing or cannot be
//!    read;
//! 3. the deployment's own documents, which a build ships and never writes;
//! 4. the local documents, the only layer ever written.
//!
//! A domain that has a document is answered by that document alone: the row
//! does not contribute to it, or a key the document leaves out would come back
//! from a value the row froze. The row is only asked for a domain with no
//! document at all, so one damaged file costs that domain and nothing else.
//!
//! The deployment's layer answers above the row rather than below it: a value a
//! deployment pins must not be shadowed by a row value frozen before it was
//! deployed. It answers whatever the local documents do, since it carries keys
//! for domains that have a local document too, and a local key wins over it.
//!
//! The domains are written one at a time, so a launcher stopped between two of
//! them leaves the domains a generation apart. They carry unrelated settings,
//! which is why that is accepted rather than paid for with a cross-file write.
//!
//! `personalized_ads` is in no document: it is the one `Settings` field marked
//! `#[serde(skip)]`, it stays a column of the row alone, and listing it here
//! would promise a document that never carries it. The row is therefore read on
//! every read and this key always comes from it: the layers above answer no part
//! of it, so a document that exists cannot lower it to the default and no
//! document's absence is needed to reach it (Q5).
//!
//! `Settings::get` still answers from the row, and nothing writes these
//! documents: both arrive with the wiring that follows. What is in place is the
//! composition a read makes, and the value a key has before a local document is
//! asked (`provided`), which is what the write path will compare against.

use serde_json::{Map, Value};

use super::{Documents, Stored, WRITES};
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
            "default_game_language",
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
    // Layer 1: the build defaults, which every key starts at.
    let mut values = default_values()?;
    // The layers above the row are laid down after it, so what they carry is
    // collected first: the row is read once, and only for the keys that need
    // it.
    let mut without_a_document: Vec<&[&str]> = Vec::new();
    let mut shipped: Vec<(&str, &[&str], Document)> = Vec::new();
    let mut local: Vec<(&str, &[&str], Document)> = Vec::new();

    for &(name, keys, schema_version) in DOMAINS {
        // Layer 3 contributes to every domain it carries a document for, not
        // only the ones with no local document.
        if let Some(document) = documents.shipped(name, schema_version).await {
            shipped.push((name, keys, document));
        }
        match documents.stored::<Document>(name, schema_version).await {
            // Layer 4: a document is the authority for the domain it exists
            // for, whichever build wrote it.
            Stored::Ready { document, .. } | Stored::Newer { document, .. } => {
                local.push((name, keys, document));
            }
            // Layer 2 reaches the result here: a domain with no document is the
            // only kind the row answers, and only for its own keys.
            Stored::Missing | Stored::Unreadable => {
                without_a_document.push(keys);
            }
        }
    }

    // The row is read whether or not a domain needs it, because
    // `personalized_ads` is in no document and only it can answer that key.
    let (row_settings, row) = row_values(handle).await?;
    for keys in without_a_document {
        contribute(&mut values, keys, &row);
    }

    // The deployment's documents sit above the row and below the local ones: a
    // value it pins must not be shadowed by a value the row froze before the
    // deployment existed, and the user's own document is the last word.
    for (name, keys, document) in shipped {
        overlay(&mut values, keys, &document, name);
    }
    for (name, keys, document) in local {
        overlay(&mut values, keys, &document, name);
    }

    let mut settings: Settings = serde_json::from_value(Value::Object(values))?;
    // `personalized_ads` is skipped when deserializing, so the map above cannot
    // carry it back; it is taken from the row the read already made.
    settings.personalized_ads = row_settings.personalized_ads;
    // A document is text the user can edit by hand, so a value out of range is
    // pulled back into it rather than handed on as it stands.
    Ok(settings.normalize())
}

/// The settings a build starts from, keyed the way a document keys them.
fn default_values() -> crate::Result<Map<String, Value>> {
    let default = serde_json::to_value(Settings::default())?;
    Ok(serde_json::from_value(default)?)
}

/// The value a key has before any local document is asked: what the deployment
/// ships for it, and what this build defaults to when the deployment ships
/// nothing.
///
/// This is what a local document is compared against, because a key holding
/// exactly this value says nothing: leaving it out of the local document reads
/// the same value and lets a later change of the deployment's document arrive.
fn provided<'a>(
    key: &str,
    config: &'a Map<String, Value>,
    defaults: &'a Map<String, Value>,
) -> Option<&'a Value> {
    config.get(key).or_else(|| defaults.get(key))
}

/// Drops what a local document repeats of a value the deployment or the build
/// already provides. Writing a value into a deployment's own document is how
/// its owner says a key should follow it, so the local copy stops shadowing it
/// as soon as the two agree, and a later change of that value still arrives.
///
/// A start runs this once, after the handover. It is not part of a read: a read
/// never writes to disk.
pub(crate) async fn prune_redundant(
    documents: &Documents<'_>,
) -> crate::Result<()> {
    // With no deployment there is nothing a local copy could shadow: what it
    // repeats is the value the build default gives it anyway.
    if !documents.ships_documents() {
        return Ok(());
    }
    let defaults = default_values()?;
    let _writing = WRITES.lock().await;
    for &(name, keys, schema_version) in DOMAINS {
        let config = documents
            .shipped::<Document>(name, schema_version)
            .await
            .unwrap_or_default();
        // A domain with no local document has nothing to drop, and one a newer
        // build wrote is not this build's to rewrite.
        let Stored::Ready { document } =
            documents.stored::<Document>(name, schema_version).await
        else {
            continue;
        };
        let mut pruned = document;
        let before = pruned.len();
        for &key in keys {
            let Some(value) = pruned.get(key).cloned() else {
                continue;
            };
            if provided(key, &config, &defaults) == Some(&value) {
                pruned.remove(key);
            }
        }
        if pruned.len() == before {
            continue;
        }
        documents.write(name, schema_version, &pruned).await?;
    }
    Ok(())
}

/// The settings as the row holds them, and the same ones keyed the way a
/// document keys them.
async fn row_values(
    handle: &Handle,
) -> crate::Result<(Settings, Map<String, Value>)> {
    let settings = Settings::get(handle).await?;
    let values = serde_json::from_value(serde_json::to_value(&settings)?)?;
    Ok((settings, values))
}

/// Lays the row's values for `keys` over `values`.
///
/// Unlike a document, nothing here can be unreadable: these values come from a
/// `Settings`, so they already carry the types their keys take.
fn contribute(
    values: &mut Map<String, Value>,
    keys: &[&str],
    row: &Map<String, Value>,
) {
    for &key in keys {
        if let Some(value) = row.get(key) {
            values.insert(key.to_string(), value.clone());
        }
    }
}

/// Lays one layer over `values`, for the keys `keys` lists only: a key outside
/// that list is ignored, and so is a value the type cannot take, which costs
/// that key and leaves what the layer below gave it. A negative number is
/// retried as zero first, so it costs no key at all.
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
        let mut unreadable = settings_error(values);

        // A negative number is the one value the type cannot take that still
        // has a nearest value inside its range. Read as zero it lands on that
        // field's lower bound once `normalize` runs, where dropping the key
        // would put back the default, which is not the nearest value to it.
        if unreadable.is_some() && is_negative_integer(value) {
            values.insert(key.to_string(), Value::from(0));
            unreadable = settings_error(values);
            if unreadable.is_none() {
                tracing::warn!(
                    document = name,
                    key,
                    "Reading a negative settings value as its lower bound"
                );
                continue;
            }
        }

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

/// Why `values` does not read as `Settings`, when they do not.
fn settings_error(values: &Map<String, Value>) -> Option<serde_json::Error> {
    serde_json::from_value::<Settings>(Value::Object(values.clone())).err()
}

/// Whether a document holds a signed integer below zero. `as_i64` also rules
/// out every other kind of value, so a string or a bool is not retried.
fn is_negative_integer(value: &Value) -> bool {
    value.as_i64().is_some_and(|number| number < 0)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::state::documents::{
        ai,
        test_support::{create_ai_tables, pool as empty_pool},
    };
    use crate::state::settings::Theme;
    use crate::state::store::tests::support::migrated_pool;

    fn document_keys() -> BTreeSet<&'static str> {
        DOMAINS
            .iter()
            .flat_map(|(_, keys, _)| keys.iter().copied())
            .collect()
    }

    /// The pair of directories a launcher reads: the one it writes, and the one
    /// a deployment ships.
    fn layered<'a>(
        local: &tempfile::TempDir,
        deployment: &tempfile::TempDir,
        pool: &'a sqlx::SqlitePool,
    ) -> Documents<'a> {
        Documents::at(local.path().to_path_buf(), pool)
            .with_config_root(Some(deployment.path().to_path_buf()))
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
    async fn a_stored_key_is_read_and_an_unreadable_value_costs_one_key() {
        let pool = migrated_pool().await;
        let handle = Handle::new(pool.clone());
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("backup.json"),
            r#"{"schema_version":1,"data":{"backup_repository_path":"/backups","nonsense":1}}"#,
        )
        .unwrap();
        std::fs::write(
            root.path().join("appearance.json"),
            r#"{"schema_version":1,"data":{"custom_background_blur":"lots","theme":"light"}}"#,
        )
        .unwrap();

        let documents = Documents::at(root.path().to_path_buf(), &pool);
        let settings = read(&documents, &handle).await.unwrap();

        assert_eq!(
            settings.backup_repository_path.as_deref(),
            Some("/backups"),
            "a key the document carries is read from it"
        );
        assert_eq!(
            settings.theme.as_str(),
            "light",
            "a key beside an unreadable one still comes from the same document"
        );
        assert_ne!(
            settings.theme.as_str(),
            Settings::default().theme.as_str(),
            "the document answers that key, not the default"
        );
        assert_eq!(
            settings.custom_background_blur,
            Settings::default().custom_background_blur,
            "a value the type cannot take costs its own key only"
        );
    }

    /// A document is text the user can edit by hand, so a number outside the
    /// range its setting holds reads back as the nearest value inside it rather
    /// than as what the file says.
    #[tokio::test]
    async fn a_hand_edited_out_of_range_value_reads_as_the_bounded_one() {
        let pool = migrated_pool().await;
        let handle = Handle::new(pool.clone());
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("appearance.json"),
            r#"{"schema_version":1,"data":{"custom_background_blur":999,"sidebar_instance_count":999}}"#,
        )
        .unwrap();
        std::fs::write(
            root.path().join("download.json"),
            r#"{"schema_version":1,"data":{"max_concurrent_downloads":0}}"#,
        )
        .unwrap();

        let documents = Documents::at(root.path().to_path_buf(), &pool);
        let settings = read(&documents, &handle).await.unwrap();

        assert_eq!(
            settings.custom_background_blur, 40,
            "a blur past its highest value reads as that value"
        );
        assert_eq!(
            settings.sidebar_instance_count, 50,
            "a sidebar count past its highest value reads as that value"
        );
        assert_eq!(
            settings.max_concurrent_downloads, 1,
            "a download count below its lowest value reads as that value"
        );
    }

    /// A negative number is the one out-of-range value a document can hold that
    /// has a nearest value inside its field: it reads as that field's lower
    /// bound, not as the default the key would fall back to if it were dropped.
    #[tokio::test]
    async fn a_negative_document_value_reads_as_the_lower_bound() {
        let pool = migrated_pool().await;
        let handle = Handle::new(pool.clone());
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("download.json"),
            r#"{"schema_version":1,"data":{"max_concurrent_downloads":-5}}"#,
        )
        .unwrap();

        let documents = Documents::at(root.path().to_path_buf(), &pool);
        let settings = read(&documents, &handle).await.unwrap();

        assert_eq!(
            settings.max_concurrent_downloads, 1,
            "a negative download count reads as its lowest value"
        );
        assert_ne!(
            settings.max_concurrent_downloads,
            Settings::default().max_concurrent_downloads,
            "the key is read rather than dropped back to its default"
        );
    }

    /// A document that leaves `data` out is a file this build cannot read, not
    /// an empty authority: the rows keep answering the domain.
    #[tokio::test]
    async fn a_document_without_data_is_unreadable() {
        let pool = migrated_pool().await;
        let handle = Handle::new(pool.clone());
        let mut row = Settings::get(&handle).await.unwrap();
        row.locale = "zh_cn".to_string();
        row.update(&handle).await.unwrap();

        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("appearance.json");
        let without_data = br#"{"schema_version":1}"#;
        std::fs::write(&path, without_data).unwrap();

        let documents = Documents::at(root.path().to_path_buf(), &pool);
        let settings = read(&documents, &handle).await.unwrap();

        assert_eq!(
            settings.locale, "zh_cn",
            "a document without `data` is unreadable, so the row answers the domain"
        );
        assert_ne!(
            settings.locale,
            Settings::default().locale,
            "the row answers that key rather than the default"
        );
        assert_eq!(
            std::fs::read(&path).unwrap().as_slice(),
            without_data.as_slice(),
            "a document that cannot be read is left alone"
        );
    }

    /// `personalized_ads` is in no document, so the row answers it even when
    /// every domain has one: the layering the other keys follow never reaches
    /// it (Q5).
    #[tokio::test]
    async fn the_ads_preference_comes_from_the_row() {
        let pool = migrated_pool().await;
        let handle = Handle::new(pool.clone());
        let mut row = Settings::get(&handle).await.unwrap();
        row.personalized_ads = true;
        row.update(&handle).await.unwrap();

        let root = tempfile::tempdir().unwrap();
        for (name, _, _) in DOMAINS {
            std::fs::write(
                root.path().join(format!("{name}.json")),
                r#"{"schema_version":1,"data":{}}"#,
            )
            .unwrap();
        }

        let documents = Documents::at(root.path().to_path_buf(), &pool);
        let settings = read(&documents, &handle).await.unwrap();

        assert!(
            settings.personalized_ads,
            "every domain has a document, and the ads preference still comes from the row"
        );
    }

    /// The row reaches the domains that have no document and stops there, so a
    /// key a document exists for cannot come back from a value the row froze.
    #[tokio::test]
    async fn the_row_answers_only_the_domains_without_a_document() {
        let pool = migrated_pool().await;
        let handle = Handle::new(pool.clone());
        let mut row = Settings::get(&handle).await.unwrap();
        row.locale = "zh_cn".to_string();
        row.backup_repository_path = Some("/from-the-row".to_string());
        row.update(&handle).await.unwrap();

        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("appearance.json"),
            r#"{"schema_version":1,"data":{"theme":"light"}}"#,
        )
        .unwrap();

        let documents = Documents::at(root.path().to_path_buf(), &pool);
        let settings = read(&documents, &handle).await.unwrap();

        assert_eq!(settings.theme.as_str(), "light", "the document answers");
        assert_eq!(
            settings.locale,
            Settings::default().locale,
            "appearance has a document, so a key it leaves out reads the \
             default rather than the row"
        );
        assert_eq!(
            settings.backup_repository_path.as_deref(),
            Some("/from-the-row"),
            "backup has no document, so the row answers for it"
        );
    }

    /// A deployment's document answers a domain no local document covers, and
    /// it answers above the row: a value the row froze cannot shadow it.
    #[tokio::test]
    async fn a_deployment_document_applies_without_a_local_one() {
        let pool = migrated_pool().await;
        let handle = Handle::new(pool.clone());
        let mut row = Settings::get(&handle).await.unwrap();
        row.theme = Theme::System;
        row.update(&handle).await.unwrap();

        let local = tempfile::tempdir().unwrap();
        let deployment = tempfile::tempdir().unwrap();
        std::fs::write(
            deployment.path().join("appearance.json"),
            r#"{"schema_version":1,"data":{"theme":"light"}}"#,
        )
        .unwrap();

        let documents = layered(&local, &deployment, &pool);
        let settings = read(&documents, &handle).await.unwrap();

        assert_eq!(
            settings.theme.as_str(),
            "light",
            "the deployment's document answers the domain the row would"
        );
        assert!(
            !local.path().join("appearance.json").exists(),
            "a read must not write the local directory"
        );
    }

    /// The local document is the last word for the keys it carries: what the
    /// deployment ships is a value below it, per key rather than per domain.
    #[tokio::test]
    async fn a_local_document_wins_over_the_deployment_one() {
        let pool = migrated_pool().await;
        let handle = Handle::new(pool.clone());
        let local = tempfile::tempdir().unwrap();
        let deployment = tempfile::tempdir().unwrap();
        std::fs::write(
            local.path().join("appearance.json"),
            r#"{"schema_version":1,"data":{"theme":"oled"}}"#,
        )
        .unwrap();
        std::fs::write(
            deployment.path().join("appearance.json"),
            r#"{"schema_version":1,"data":{"theme":"light","locale":"zh_cn"}}"#,
        )
        .unwrap();

        let documents = layered(&local, &deployment, &pool);
        let settings = read(&documents, &handle).await.unwrap();

        assert_eq!(
            settings.theme.as_str(),
            "oled",
            "the key the local document carries overrides the deployment's"
        );
        assert_eq!(
            settings.locale, "zh_cn",
            "a key it leaves out still comes from the deployment"
        );
    }

    /// A deployment's document this build cannot read is a layer that is not
    /// there: the row answers that domain and the file is left as it stands.
    #[tokio::test]
    async fn a_corrupt_deployment_document_is_ignored() {
        let pool = migrated_pool().await;
        let handle = Handle::new(pool.clone());
        let row = Settings::get(&handle).await.unwrap();
        let local = tempfile::tempdir().unwrap();
        let deployment = tempfile::tempdir().unwrap();
        let without_data = br#"{"schema_version":1,"theme":"light"}"#;
        std::fs::write(deployment.path().join("appearance.json"), without_data)
            .unwrap();
        std::fs::write(
            deployment.path().join("download.json"),
            r#"{"schema_version":1,"data":"nonsense"}"#,
        )
        .unwrap();
        std::fs::write(
            deployment.path().join("backup.json"),
            r#"{"schema_version":1,"data":{"backup_repository_path":"/shipped"}}"#,
        )
        .unwrap();

        let documents = layered(&local, &deployment, &pool);
        let settings = read(&documents, &handle).await.unwrap();

        assert_eq!(
            settings.backup_repository_path.as_deref(),
            Some("/shipped"),
            "one document this build cannot read costs its own domain only"
        );
        assert_eq!(
            settings.theme.as_str(),
            row.theme.as_str(),
            "a document without `data` is unreadable, so the row answers"
        );
        assert_eq!(
            settings.max_concurrent_downloads, row.max_concurrent_downloads,
            "`data` that is not an object is unreadable too"
        );
        assert_eq!(
            std::fs::read(deployment.path().join("appearance.json"))
                .unwrap()
                .as_slice(),
            without_data.as_slice(),
            "a read leaves a document it cannot interpret as it stands"
        );
    }

    /// The value a key reads before a local document is asked: what the
    /// deployment ships, and the build's own default when it ships nothing.
    #[test]
    fn provided_prefers_the_deployment_value() {
        let defaults = default_values().unwrap();
        let config =
            Map::from_iter([("theme".to_string(), Value::from("light"))]);

        assert_ne!(
            config.get("theme"),
            defaults.get("theme"),
            "the two layers disagree about the key"
        );
        assert_eq!(
            provided("theme", &config, &defaults),
            config.get("theme"),
            "the deployment's value comes first"
        );
        assert_eq!(
            provided("locale", &config, &defaults),
            defaults.get("locale"),
            "a key it ships nothing for falls back to the build's default"
        );
        assert_eq!(
            provided("no_such_key", &config, &defaults),
            None,
            "a key no layer carries has no provided value"
        );
    }

    /// The deployment's directory belongs to the deployment: a write lands in
    /// the local one, even for a domain the deployment ships a document for.
    #[cfg(unix)]
    #[tokio::test]
    async fn writing_never_touches_the_deployment_directory() {
        use std::os::unix::fs::PermissionsExt;

        let pool = empty_pool().await;
        create_ai_tables(&pool).await;
        let local = tempfile::tempdir().unwrap();
        let deployment = tempfile::tempdir().unwrap();
        let shipped = deployment.path().join("ai.json");
        let contents = br#"{"schema_version":1,"data":{"enabled":false}}"#;
        std::fs::write(&shipped, contents).unwrap();
        let modified = std::fs::metadata(&shipped).unwrap().modified().unwrap();
        std::fs::set_permissions(
            deployment.path(),
            std::fs::Permissions::from_mode(0o555),
        )
        .unwrap();

        let documents = layered(&local, &deployment, &pool);
        let written = ai::set_enabled(&documents, true).await;

        // Restored before anything is asserted: a directory left read-only
        // would keep the temporary one from being removed afterwards.
        std::fs::set_permissions(
            deployment.path(),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();

        written.unwrap();
        assert!(
            local.path().join("ai.json").is_file(),
            "the write lands in the local directory"
        );
        assert!(ai::read(&documents).await.unwrap().enabled);
        assert_eq!(
            std::fs::read(&shipped).unwrap().as_slice(),
            contents.as_slice(),
            "a write must leave the deployment's document byte for byte"
        );
        assert_eq!(
            std::fs::metadata(&shipped).unwrap().modified().unwrap(),
            modified,
            "a write must not touch the deployment's document at all"
        );
        let listed = std::fs::read_dir(deployment.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(listed, [std::ffi::OsString::from("ai.json")]);
    }

    /// A start drops the local entries a provided value already covers, so a
    /// change of that value reaches the settings again.
    #[tokio::test]
    async fn startup_drops_the_entries_a_deployment_value_provides() {
        let pool = migrated_pool().await;
        let local = tempfile::tempdir().unwrap();
        let deployment = tempfile::tempdir().unwrap();
        let written = local.path().join("appearance.json");
        std::fs::write(
            &written,
            r#"{"schema_version":1,"data":{"theme":"light","locale":"zh_cn"}}"#,
        )
        .unwrap();
        std::fs::write(
            deployment.path().join("appearance.json"),
            r#"{"schema_version":1,"data":{"theme":"light"}}"#,
        )
        .unwrap();

        let documents = layered(&local, &deployment, &pool);
        prune_redundant(&documents).await.unwrap();

        let stored: Value =
            serde_json::from_slice(&std::fs::read(&written).unwrap()).unwrap();
        let data = stored["data"].as_object().unwrap();
        assert!(
            !data.contains_key("theme"),
            "the key the deployment provides is dropped from the local document"
        );
        assert_eq!(
            data.get("locale"),
            Some(&Value::from("zh_cn")),
            "a key only the user set stays where it is"
        );

        let settings =
            read(&documents, &Handle::new(pool.clone())).await.unwrap();
        assert_eq!(
            settings.theme.as_str(),
            "light",
            "the deployment still answers the key the local copy stopped carrying"
        );
        assert_eq!(settings.locale, "zh_cn");
    }

    /// With no deployment there is nothing a local copy could shadow, so a
    /// start rewrites nothing: the value it repeats is the default's own.
    #[tokio::test]
    async fn startup_leaves_the_documents_alone_without_a_deployment() {
        let pool = migrated_pool().await;
        let local = tempfile::tempdir().unwrap();
        let written = local.path().join("appearance.json");
        let contents = br#"{"schema_version":1,"data":{"theme":"dark"}}"#;
        std::fs::write(&written, contents).unwrap();

        let documents = Documents::at(local.path().to_path_buf(), &pool);
        prune_redundant(&documents).await.unwrap();

        assert_eq!(
            std::fs::read(&written).unwrap().as_slice(),
            contents.as_slice(),
            "no deployment means no repeated value to drop"
        );
    }
}
