//! Theseus settings management interface

pub use crate::{
    State,
    state::{
        DownloadSourceMode, Hooks, MemorySettings, PrivacySettings, Settings,
        WindowSize,
    },
};

/// Serializes a read-modify-write of the settings, which the store only
/// serializes per document.
static SETTINGS_WRITE: tokio::sync::Mutex<()> =
    tokio::sync::Mutex::const_new(());

fn merge_settings(
    current: &mut serde_json::Value,
    patch: serde_json::Value,
) -> crate::Result<()> {
    if let (Some(target), Some(changes)) =
        (current.as_object_mut(), patch.as_object())
    {
        for (key, value) in changes {
            let existing = target.get_mut(key).ok_or_else(|| {
                crate::ErrorKind::InputError(format!("Unknown setting {key}"))
            })?;
            if matches!(key.as_str(), "feature_flags" | "memory" | "hooks")
                && existing.is_object()
                && value.is_object()
            {
                existing
                    .as_object_mut()
                    .unwrap()
                    .extend(value.as_object().unwrap().clone());
            } else {
                *existing = value.clone();
            }
        }
        Ok(())
    } else {
        Err(crate::ErrorKind::InputError(
            "Settings patch must be an object".to_string(),
        )
        .into())
    }
}

/// Merges field-level changes while serializing settings writes.
pub async fn patch(patch: serde_json::Value) -> crate::Result<Settings> {
    let _guard = SETTINGS_WRITE.lock().await;
    let state = State::get().await?;
    let settings = apply_patch(patch).await?;
    state.apply_runtime_settings(&settings).await?;
    Ok(settings)
}

/// Applies a patch to the documents and returns the settings they describe.
async fn apply_patch(patch: serde_json::Value) -> crate::Result<Settings> {
    let current = Settings::get().await;
    let mut value = serde_json::to_value(&current)?;
    merge_settings(&mut value, patch)?;
    let mut settings: Settings = serde_json::from_value(value)?;
    settings.telemetry = current.telemetry;
    settings.telemetry_consent_version = current.telemetry_consent_version;
    settings.discord_rpc = current.discord_rpc;
    super::terracotta::validate_public_nodes(
        &settings.terracotta_public_nodes,
    )?;
    crate::util::download_dns::DownloadDnsResolver::with_doh(
        settings.doh_enabled,
    )?;
    settings.apply_legacy_download_source_settings();
    settings.update().await;
    Ok(Settings::get().await)
}

/// Applies a probe result only while its source mode is still selected, preserving concurrent edits.
pub(crate) async fn apply_mojang_auth_source(
    expected_source: DownloadSourceMode,
    use_mirror: bool,
) -> crate::Result<bool> {
    let _guard = SETTINGS_WRITE.lock().await;
    let state = State::get().await?;
    let Some(use_mirror) =
        apply_mojang_auth_probe(expected_source, use_mirror).await?
    else {
        return Ok(false);
    };
    state.set_mojang_auth_use_mirror(use_mirror);
    Ok(true)
}

/// The mirror the probe settles on, or nothing when the mode it was started
/// under is no longer the one the settings carry.
async fn apply_mojang_auth_probe(
    expected_source: DownloadSourceMode,
    use_mirror: bool,
) -> crate::Result<Option<bool>> {
    let mut settings = Settings::get().await;
    if settings.mojang_auth_source != expected_source {
        return Ok(None);
    }
    // Explicit choices cannot be overridden by a delayed automatic probe.
    let use_mirror = match expected_source {
        DownloadSourceMode::MirrorPreferred => true,
        DownloadSourceMode::OfficialOnly => false,
        DownloadSourceMode::Auto | DownloadSourceMode::OfficialPreferred => {
            use_mirror
        }
    };
    let args = crate::util::mojang::auth_proxy_java_args(
        &settings.extra_launch_args,
        use_mirror,
    );
    if args != settings.extra_launch_args {
        settings.extra_launch_args = args;
        settings.update().await;
    }
    Ok(Some(use_mirror))
}

/// Gets entire settings
#[tracing::instrument]
pub async fn get() -> crate::Result<Settings> {
    State::get().await?;
    Ok(Settings::get().await)
}

#[tracing::instrument]
pub async fn get_privacy() -> crate::Result<PrivacySettings> {
    State::get().await?;
    Ok(Settings::privacy().await)
}

#[tracing::instrument]
pub async fn set_privacy(
    privacy: PrivacySettings,
) -> crate::Result<PrivacySettings> {
    let state = State::get().await?;
    // The queue is dropped before the preference is stored, so a worker that
    // runs in between cannot upload the events this change discards.
    let mut transaction = state.pool.begin().await?;
    sqlx::query("DELETE FROM telemetry_outbox")
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;
    Settings::set_privacy(&privacy).await;

    if let Err(error) =
        crate::telemetry::set_enabled(&state, privacy.telemetry).await
    {
        tracing::debug!(target: "theseus::telemetry", %error, "Failed to apply telemetry state");
    }
    if let Err(error) = state.discord_rpc.clear_to_default(true).await {
        tracing::debug!(target: "theseus::telemetry", %error, "Failed to apply Discord RPC state");
    }
    get_privacy().await
}

#[tracing::instrument]
pub async fn set_telemetry(enabled: bool) -> crate::Result<PrivacySettings> {
    let state = State::get().await?;
    // The queue is dropped before the preference is stored, so a worker that
    // runs in between cannot upload the events this change discards.
    let mut transaction = state.pool.begin().await?;
    sqlx::query("DELETE FROM telemetry_outbox")
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;
    Settings::set_telemetry(enabled).await;
    if let Err(error) = crate::telemetry::set_enabled(&state, enabled).await {
        tracing::debug!(target: "theseus::telemetry", %error, "Failed to apply telemetry state");
    }
    get_privacy().await
}

#[tracing::instrument]
pub async fn set_discord_rpc(enabled: bool) -> crate::Result<PrivacySettings> {
    let state = State::get().await?;
    Settings::set_discord_rpc(enabled).await;
    if let Err(error) = state.discord_rpc.clear_to_default(true).await {
        tracing::debug!(target: "theseus::telemetry", %error, "Failed to apply Discord RPC state");
    }
    get_privacy().await
}

#[tracing::instrument]
pub async fn cancel_directory_change(
    app_identifier: &str,
) -> crate::Result<()> {
    // This is called to handle state initialization errors due to folder migrations
    // failing, so fetching a DB connection pool from `State::get` is not reliable here
    crate::state::db::connect(app_identifier).await?;
    let mut settings = Settings::get().await;

    if let Some(prev_custom_dir) = settings.prev_custom_dir {
        settings.prev_custom_dir = None;
        settings.custom_dir = Some(prev_custom_dir);
    }

    settings.update().await;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn mojang_probe_updates_preserve_arguments_and_reject_stale_modes() {
        apply_patch(json!({"extra_launch_args": ["-Xmx4G", "-Dcustom=keep"], "mojang_auth_source": "auto"})).await.unwrap();
        assert_eq!(
            apply_mojang_auth_probe(DownloadSourceMode::Auto, true)
                .await
                .unwrap(),
            Some(true)
        );
        let mirrored = Settings::get().await;
        assert_eq!(mirrored.extra_launch_args.len(), 7);
        assert!(
            mirrored
                .extra_launch_args
                .contains(&"-Dcustom=keep".to_string())
        );
        apply_patch(json!({"extra_launch_args": ["-Dnew=user-edit"], "mojang_auth_source": "official_only"})).await.unwrap();
        assert_eq!(
            apply_mojang_auth_probe(DownloadSourceMode::Auto, true)
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            Settings::get().await.extra_launch_args,
            ["-Dnew=user-edit"]
        );
        // An explicit official mode also ignores an inconsistent mirror request.
        assert_eq!(
            apply_mojang_auth_probe(DownloadSourceMode::OfficialOnly, true)
                .await
                .unwrap(),
            Some(false)
        );
        assert_eq!(
            Settings::get().await.extra_launch_args,
            ["-Dnew=user-edit"]
        );
        apply_patch(json!({"mojang_auth_source": "mirror_preferred"}))
            .await
            .unwrap();
        assert_eq!(
            apply_mojang_auth_probe(DownloadSourceMode::MirrorPreferred, false)
                .await
                .unwrap(),
            Some(true)
        );
        assert_eq!(Settings::get().await.extra_launch_args.len(), 6);
        apply_patch(json!({"mojang_auth_source": "official_preferred"}))
            .await
            .unwrap();
        assert_eq!(
            apply_mojang_auth_probe(
                DownloadSourceMode::OfficialPreferred,
                false
            )
            .await
            .unwrap(),
            Some(false)
        );
        assert_eq!(
            Settings::get().await.extra_launch_args,
            ["-Dnew=user-edit"]
        );
    }

    #[tokio::test]
    async fn settings_patch_preserves_unrelated_fields_and_nested_flags() {
        let initial = Settings::get().await;
        apply_patch(json!({"ui_font": "Test Font", "feature_flags": {"page_transitions": false}})).await.unwrap();
        apply_patch(json!({"developer_mode": true, "feature_flags": {"show_instance_play_time": false}})).await.unwrap();
        let saved = Settings::get().await;
        assert_eq!(saved.ui_font.as_deref(), Some("Test Font"));
        assert!(saved.developer_mode);
        let flags = serde_json::to_value(&saved.feature_flags).unwrap();
        assert_eq!(flags["page_transitions"], false);
        assert_eq!(flags["show_instance_play_time"], false);
        assert_eq!(saved.locale, initial.locale);
        let saved = apply_patch(json!({"ui_font": null})).await.unwrap();
        assert_eq!(saved.ui_font, None);
        assert!(saved.developer_mode);
    }

    #[tokio::test]
    async fn settings_patch_rejects_invalid_fields_without_writing() {
        let initial = Settings::get().await;
        for patch in [
            json!({"unknown_field": true}),
            json!({"developer_mode": "bad"}),
            json!([]),
        ] {
            assert!(apply_patch(patch).await.is_err());
        }
        let saved = Settings::get().await;
        assert_eq!(
            serde_json::to_value(saved).unwrap(),
            serde_json::to_value(initial).unwrap()
        );
    }

    #[tokio::test]
    async fn settings_patch_does_not_override_privacy_consent() {
        let initial = Settings::get().await;
        let saved = apply_patch(json!({"telemetry": !initial.telemetry, "discord_rpc": !initial.discord_rpc, "telemetry_consent_version": 999})).await.unwrap();
        assert_eq!(saved.telemetry, initial.telemetry);
        assert_eq!(saved.discord_rpc, initial.discord_rpc);
        assert_eq!(
            saved.telemetry_consent_version,
            initial.telemetry_consent_version
        );
    }

    #[tokio::test]
    async fn settings_patch_serializes_concurrent_regions() {
        let save = |patch| async {
            let _guard = SETTINGS_WRITE.lock().await;
            apply_patch(patch).await.unwrap();
        };
        tokio::join!(
            save(json!({"ui_font": "Concurrent Font"})),
            save(json!({"developer_mode": true}))
        );
        let saved = Settings::get().await;
        assert_eq!(saved.ui_font.as_deref(), Some("Concurrent Font"));
        assert!(saved.developer_mode);
    }
}
