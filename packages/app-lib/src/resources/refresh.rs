//! Instance-owned refresh work, independent of any window subscription.

mod entry;

use super::*;
use crate::state::store::{
    content_refresh::RefreshWrite, content_snapshot::SnapshotRecords,
};
use futures::{StreamExt, stream::FuturesUnordered};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::Arc,
};
use tokio::sync::{Mutex, broadcast, mpsc};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefreshMode {
    Normal,
    ForceRemote,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RefreshTask {
    pub id: String,
    pub mode: RefreshMode,
    pub running: bool,
    pub scanned: usize,
    pub checked: usize,
    pub failed: usize,
    pub unresolved: usize,
    pub errors: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RefreshEvent {
    pub instance_id: String,
    pub task_id: String,
    pub sequence: u64,
    pub patch: RefreshPatch,
    pub task: RefreshTask,
}

#[derive(Clone, Debug, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum RefreshPatch {
    Content {
        snapshot: crate::state::InstanceContentSnapshot,
    },
    Entry {
        entry_id: String,
        file_id: String,
        sha1: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        artifact_ref: Option<SourceArtifactRef>,
        #[serde(skip_serializing_if = "Option::is_none")]
        presentation: Option<crate::state::ContentItem>,
        #[serde(skip_serializing_if = "Option::is_none")]
        update: Option<RefreshUpdate>,
    },
    Progress,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RefreshUpdate {
    pub candidate: Option<SourceReleaseRef>,
    pub failed: bool,
    pub unresolved: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RefreshState {
    pub snapshot: crate::state::InstanceContentSnapshot,
    pub task: Option<RefreshTask>,
    pub sequence: u64,
}

struct Slot {
    task: RefreshTask,
    sequence: u64,
    force_pending: bool,
    cancel: CancellationToken,
}

pub(crate) struct RefreshCoordinator {
    tasks: parking_lot::Mutex<Vec<tokio::task::JoinHandle<()>>>,
    slots: Mutex<HashMap<String, Slot>>,
    events: broadcast::Sender<RefreshEvent>,
    shutdown: CancellationToken,
}

impl Default for RefreshCoordinator {
    fn default() -> Self {
        Self {
            tasks: Default::default(),
            slots: Mutex::new(HashMap::new()),
            events: broadcast::channel(1024).0,
            shutdown: CancellationToken::new(),
        }
    }
}

impl RefreshCoordinator {
    pub(crate) async fn invalidate(&self, instance_id: &str) {
        if let Some(slot) = self.slots.lock().await.get_mut(instance_id) {
            slot.force_pending = false;
            slot.cancel.cancel();
        }
    }
    pub(crate) fn subscribe(&self) -> broadcast::Receiver<RefreshEvent> {
        self.events.subscribe()
    }

    pub(crate) async fn shutdown(&self) {
        self.shutdown.cancel();
        let tasks = std::mem::take(&mut *self.tasks.lock());
        for mut task in tasks {
            if tokio::time::timeout(
                std::time::Duration::from_secs(3),
                &mut task,
            )
            .await
            .is_err()
            {
                task.abort();
            }
        }
    }

    pub(crate) async fn start(
        &self,
        state: Arc<crate::State>,
        instance_id: String,
        mode: RefreshMode,
    ) -> ResourceResult<RefreshTask> {
        if self.shutdown.is_cancelled() {
            return Err(ResourceError::Canceled);
        }
        let mut slots = self.slots.lock().await;
        if let Some(slot) = slots.get_mut(&instance_id)
            && slot.task.running
            && !slot.cancel.is_cancelled()
        {
            if mode == RefreshMode::ForceRemote
                && slot.task.mode == RefreshMode::Normal
            {
                slot.force_pending = true;
            }
            return Ok(slot.task.clone());
        }
        let sequence = slots.get(&instance_id).map_or(0, |slot| slot.sequence);
        let task = RefreshTask {
            id: uuid::Uuid::new_v4().to_string(),
            mode,
            running: true,
            scanned: 0,
            checked: 0,
            failed: 0,
            unresolved: 0,
            errors: vec![],
        };
        let cancel = self.shutdown.child_token();
        slots.insert(
            instance_id.clone(),
            Slot {
                task: task.clone(),
                sequence,
                force_pending: false,
                cancel: cancel.clone(),
            },
        );
        drop(slots);
        let id = task.id.clone();
        let handle = tokio::spawn(async move {
            let coordinator = &state.content_refresh;
            let mut current_mode = mode;
            loop {
                let result =
                    run_round(&state, &instance_id, &id, current_mode, &cancel)
                        .await;
                let mut slots = coordinator.slots.lock().await;
                let Some(slot) = slots
                    .get_mut(&instance_id)
                    .filter(|slot| slot.task.id == id)
                else {
                    return;
                };
                if let Err(error) = result {
                    slot.task.errors.push(error.to_string());
                    slot.task.failed += 1;
                }
                if slot.force_pending && !cancel.is_cancelled() {
                    slot.force_pending = false;
                    slot.task.mode = RefreshMode::ForceRemote;
                    slot.task.scanned = 0;
                    slot.task.checked = 0;
                    slot.task.failed = 0;
                    slot.task.unresolved = 0;
                    slot.task.errors.clear();
                    current_mode = RefreshMode::ForceRemote;
                    continue;
                }
                slot.task.running = false;
                coordinator.publish(&instance_id, slot, RefreshPatch::Progress);
                break;
            }
        });
        let mut tasks = self.tasks.lock();
        tasks.retain(|task| !task.is_finished());
        tasks.push(handle);
        Ok(task)
    }

    pub(crate) async fn state(
        &self,
        state: &crate::State,
        instance_id: &str,
    ) -> ResourceResult<RefreshState> {
        let slots = self.slots.lock().await;
        let snapshot =
            crate::state::instances::commands::get_local_content_snapshot(
                instance_id,
                state,
            )
            .await?;
        let slot = slots.get(instance_id);
        Ok(RefreshState {
            snapshot,
            task: slot.map(|slot| slot.task.clone()),
            sequence: slot.map_or(0, |slot| slot.sequence),
        })
    }

    fn publish(&self, instance_id: &str, slot: &mut Slot, patch: RefreshPatch) {
        slot.sequence += 1;
        let _ = self.events.send(RefreshEvent {
            instance_id: instance_id.into(),
            task_id: slot.task.id.clone(),
            sequence: slot.sequence,
            patch,
            task: slot.task.clone(),
        });
    }

    async fn save(
        &self,
        state: &crate::State,
        instance_id: &str,
        task_id: &str,
        writes: Vec<RefreshWrite>,
    ) -> ResourceResult<()> {
        let mut slots = self.slots.lock().await;
        let Some(slot) = slots
            .get_mut(instance_id)
            .filter(|slot| slot.task.id == task_id)
        else {
            return Ok(());
        };
        if slot.cancel.is_cancelled() {
            return Err(ResourceError::Canceled);
        }
        let mut current = Vec::new();
        let _lock = state.lock_instance_content_exclusive(instance_id).await;
        for write in writes {
            if entry::bytes_still_current(state, instance_id, &write).await? {
                current.push(write);
            }
        }
        let applied = crate::state::store::content_refresh::set_results(
            &state.data,
            instance_id,
            current,
        )
        .await?;
        for write in applied {
            tracing::debug!(instance_id, entry_id = %write.entry_id, update = write.check.is_some(), presentation = write.presentation.is_some(), "Saved content refresh result");
            let update = write.check.as_ref().map(|check| {
                use crate::state::store::content_refresh_checks::CheckAttempt;
                slot.task.checked += 1;
                slot.task.failed +=
                    usize::from(check.attempt == CheckAttempt::Failed);
                slot.task.unresolved +=
                    usize::from(check.attempt == CheckAttempt::Unrecognized);
                RefreshUpdate {
                    candidate: check
                        .last_success
                        .as_ref()
                        .and_then(|success| success.candidate.clone()),
                    failed: check.attempt == CheckAttempt::Failed,
                    unresolved: check.attempt == CheckAttempt::Unrecognized,
                }
            });
            self.publish(
                instance_id,
                slot,
                RefreshPatch::Entry {
                    entry_id: write.entry_id,
                    file_id: write.file_id,
                    sha1: write.context.sha1,
                    artifact_ref: write
                        .artifact
                        .map(|record| record.artifact.reference),
                    presentation: write
                        .presentation
                        .map(|record| record.content),
                    update,
                },
            );
        }
        Ok(())
    }
}

async fn run_round(
    state: &Arc<crate::State>,
    instance_id: &str,
    task_id: &str,
    mode: RefreshMode,
    cancel: &CancellationToken,
) -> ResourceResult<()> {
    let started = std::time::Instant::now();
    let service = state.resources().await?.clone();
    let (scan_tx, mut scans) = mpsc::channel(8);
    let scan_state = state.clone();
    let scan_instance = instance_id.to_owned();
    let scan_cancel = cancel.child_token();
    let _cancel_on_exit = scan_cancel.clone().drop_guard();
    let worker_cancel = scan_cancel.clone();
    let mut scan =
        tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
            crate::state::instances::commands::sync_content_files_progressively(
                &scan_instance,
                &scan_state,
                &scan_tx,
                &worker_cancel,
            )
            .await
        }));
    let (writes_tx, mut writes_rx) = mpsc::channel::<RefreshWrite>(64);
    let mut workers = FuturesUnordered::new();
    let mut pending = VecDeque::new();
    let mut scheduled = HashSet::new();
    let mut buffered = Vec::new();
    let mut interval =
        tokio::time::interval(std::time::Duration::from_millis(50));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut scan_done = false;
    let mut scans_closed = false;
    let mut failed_paths = HashSet::new();
    loop {
        while workers.len() < 8 {
            let Some(item) = pending.pop_front() else {
                break;
            };
            let worker_state = state.clone();
            let worker_service = service.clone();
            let worker_cancel = scan_cancel.clone();
            let worker_tx = writes_tx.clone();
            workers.push(tokio_util::task::AbortOnDropHandle::new(
                tokio::spawn(async move {
                    entry::run(
                        &worker_state,
                        worker_service,
                        item,
                        mode,
                        &worker_cancel,
                        worker_tx,
                    )
                    .await
                }),
            ));
        }
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(ResourceError::Canceled),
            _ = interval.tick() => {
                if !buffered.is_empty() { state.content_refresh.save(state, instance_id, task_id, std::mem::take(&mut buffered)).await?; }
            }
            Some(write) = writes_rx.recv() => {
                buffered.push(write);
                if buffered.len() >= 32 { state.content_refresh.save(state, instance_id, task_id, std::mem::take(&mut buffered)).await?; }
            }
            Some(result) = workers.next(), if !workers.is_empty() => {
                if let Err(error) = result.unwrap_or_else(|error| Err(ResourceError::InvalidRequest(error.to_string()))) {
                    let mut slots = state.content_refresh.slots.lock().await;
                    if let Some(slot) = slots.get_mut(instance_id).filter(|slot| slot.task.id == task_id) {
                        slot.task.failed += 1;
                        slot.task.errors.push(error.to_string());
                        state.content_refresh.publish(instance_id, slot, RefreshPatch::Progress);
                    }
                }
            }
            progress = scans.recv(), if !scans_closed => {
                match progress {
                    Some(crate::state::instances::commands::ScanProgress::Files(files)) => {
                        let mut slots = state.content_refresh.slots.lock().await;
                        let records = crate::state::store::content_snapshot::get(&state.data, instance_id).await?;
                        let work = entry::work(&records, files.iter().filter(|file| !failed_paths.contains(&file.relative_path)).map(|file| file.id.as_str()), mode);
                        for item in work {
                            if scheduled.insert((item.entry.entry_id.clone(), item.context.sha1.clone())) {
                                pending.push_back(item);
                            }
                        }
                        let snapshot = crate::state::instances::commands::content_snapshot::local::project(records);
                        if let Some(slot) = slots.get_mut(instance_id).filter(|slot| slot.task.id == task_id) {
                            slot.task.scanned = snapshot.items.len();
                            state.content_refresh.publish(instance_id, slot, RefreshPatch::Content { snapshot });
                        }
                    }
                    Some(crate::state::instances::commands::ScanProgress::Failed { path, error }) => {
                        if !failed_paths.insert(path.clone()) { continue; }
                        let mut slots = state.content_refresh.slots.lock().await;
                        if let Some(slot) = slots.get_mut(instance_id) { slot.task.failed += 1; slot.task.errors.push(format!("{path}: {error}")); }
                    }
                    None => { scans_closed = true; }
                }
            }
            result = &mut scan, if !scan_done => { result.map_err(|error| ResourceError::InvalidRequest(error.to_string()))??; scan_done = true; }
        }
        if scan_done
            && scans.is_empty()
            && workers.is_empty()
            && pending.is_empty()
            && writes_rx.is_empty()
        {
            if !buffered.is_empty() {
                state
                    .content_refresh
                    .save(
                        state,
                        instance_id,
                        task_id,
                        std::mem::take(&mut buffered),
                    )
                    .await?;
            }
            break;
        }
    }
    tracing::debug!(
        instance_id,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "Completed content refresh round"
    );
    Ok(())
}

pub async fn start(
    instance_id: String,
    mode: RefreshMode,
) -> ResourceResult<RefreshTask> {
    let state = crate::State::get().await?;
    state
        .content_refresh
        .start(state.clone(), instance_id, mode)
        .await
}

pub async fn get_state(instance_id: String) -> ResourceResult<RefreshState> {
    let state = crate::State::get().await?;
    state.content_refresh.state(&state, &instance_id).await
}

pub async fn subscribe() -> ResourceResult<broadcast::Receiver<RefreshEvent>> {
    Ok(crate::State::get().await?.content_refresh.subscribe())
}

#[cfg(test)]
#[path = "../state/store/tests/content_refresh_runtime.rs"]
mod tests;
