//! What the sync checkpoints collection promises.

use super::support;
use crate::state::store::Collection as _;
use crate::state::store::Handle;
use crate::state::store::atom;
use crate::state::store::instance_sync_checkpoints::{
    CheckpointKey, CheckpointRecord, CheckpointScope, CheckpointStatus,
    InstanceSyncCheckpoints,
};

async fn handle(instance: &str) -> Handle {
    let pool = support::pool().await;
    support::add_instance(&pool, instance).await;
    support::enable_feature(&pool, instance, "game_options").await;
    Handle::new(pool)
}

fn key(variant: &str) -> CheckpointKey {
    CheckpointKey {
        instance_id: "instance".to_string(),
        feature: "game_options".to_string(),
        variant: variant.to_string(),
    }
}

fn pending() -> CheckpointRecord {
    CheckpointRecord {
        expected_sha1: "abc".to_string(),
        merge_base: None,
        source_revision: 1,
        status: CheckpointStatus::Pending,
        link_mode: None,
    }
}

#[tokio::test]
async fn a_checkpoint_finishes_inside_one_unit() {
    let handle = handle("instance").await;
    let checkpoint = key("default");
    InstanceSyncCheckpoints::set(&handle, &checkpoint, Some(&pending()))
        .await
        .unwrap();

    let mut unit = atom(&handle).await.unwrap();
    let mut record = InstanceSyncCheckpoints::atom_get(&mut unit, &checkpoint)
        .await
        .unwrap()
        .expect("checkpoint");
    record.status = CheckpointStatus::Ready;
    record.link_mode = Some("hard".to_string());
    InstanceSyncCheckpoints::atom_set(&mut unit, &checkpoint, Some(&record))
        .await
        .unwrap();
    unit.commit().await.unwrap();

    let stored = InstanceSyncCheckpoints::get(&handle, &checkpoint)
        .await
        .unwrap()
        .expect("checkpoint");
    assert_eq!(stored.status, CheckpointStatus::Ready);
    assert_eq!(stored.link_mode.as_deref(), Some("hard"));
}

#[tokio::test]
async fn a_scope_clears_its_own_feature_only() {
    let handle = handle("instance").await;
    let scope = CheckpointScope {
        instance_id: "instance".to_string(),
        feature: "game_options".to_string(),
    };
    InstanceSyncCheckpoints::set(&handle, &key("default"), Some(&pending()))
        .await
        .unwrap();
    InstanceSyncCheckpoints::set(&handle, &key("legacy"), Some(&pending()))
        .await
        .unwrap();

    InstanceSyncCheckpoints::set_scoped(&handle, &scope, &[])
        .await
        .unwrap();

    assert!(
        InstanceSyncCheckpoints::get_all(&handle)
            .await
            .unwrap()
            .is_empty()
    );
}
