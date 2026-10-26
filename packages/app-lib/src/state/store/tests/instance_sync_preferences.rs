//! What the instance sync preferences collection promises.

use super::support;
use crate::state::store::Collection as _;
use crate::state::store::Handle;
use crate::state::store::instance_sync_preferences::{
    InstancePreferenceKey, InstancePreferenceRecord, InstanceSyncPreferences,
};

fn handle_of(pool: &sqlx::SqlitePool) -> Handle {
    Handle::new(pool.clone())
}

fn key(instance_id: &str, feature: &str) -> InstancePreferenceKey {
    InstancePreferenceKey {
        instance_id: instance_id.to_string(),
        feature: feature.to_string(),
    }
}

fn enabled() -> InstancePreferenceRecord {
    InstancePreferenceRecord { enabled: true }
}

#[tokio::test]
async fn a_preference_round_trips_and_clears() {
    let pool = support::pool().await;
    support::add_instance(&pool, "instance").await;
    let handle = handle_of(&pool);
    let preference = key("instance", "screenshots");

    InstanceSyncPreferences::set(&handle, &preference, Some(&enabled()))
        .await
        .unwrap();
    assert_eq!(
        InstanceSyncPreferences::get(&handle, &preference)
            .await
            .unwrap(),
        Some(enabled())
    );

    InstanceSyncPreferences::set(&handle, &preference, None)
        .await
        .unwrap();
    assert_eq!(
        InstanceSyncPreferences::get(&handle, &preference)
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn a_scope_reads_only_that_instances_preferences() {
    let pool = support::pool().await;
    support::add_instance(&pool, "first").await;
    support::add_instance(&pool, "second").await;
    let handle = handle_of(&pool);
    for (instance_id, feature) in [
        ("first", "screenshots"),
        ("second", "screenshots"),
        ("first", "game_options"),
    ] {
        InstanceSyncPreferences::set(
            &handle,
            &key(instance_id, feature),
            Some(&enabled()),
        )
        .await
        .unwrap();
    }

    let scoped =
        InstanceSyncPreferences::get_scoped(&handle, &"first".to_string())
            .await
            .unwrap();

    assert_eq!(scoped.len(), 2);
    assert!(scoped.iter().all(|(key, _)| key.instance_id == "first"));
}
