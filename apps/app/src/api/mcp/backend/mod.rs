mod backups;
mod downloads;
mod instances;
mod java;
mod jobs;
mod launch;
mod lifecycle;
mod logs;
mod metadata;
mod queries;
mod resources;
mod worlds;

use futures::future::BoxFuture;
use launcher_mcp::{
    Backend, BackendContext, Command, ScopedRegistry, ToolError,
};
use serde::Serialize;
use serde_json::{Value, json};

#[derive(Default)]
pub(super) struct LauncherBackend {
    plans: ScopedRegistry<theseus::resources::ResourceOperationPlan>,
    restores: ScopedRegistry<backups::RestorePreview>,
}

impl Backend for LauncherBackend {
    fn execute(
        &self,
        command: Command,
        context: BackendContext,
    ) -> BoxFuture<'_, Result<Value, ToolError>> {
        Box::pin(async move {
            match command {
                command @ (Command::DownloadJobList { .. }
                | Command::DownloadJobGet { .. }
                | Command::DownloadJobGetItems { .. }
                | Command::DownloadJobRetry { .. }
                | Command::DownloadJobResume { .. }
                | Command::DownloadJobCancel { .. }
                | Command::DownloadJobDelete { .. }
                | Command::DownloadHistoryClear {}
                | Command::InstallJobMissingFiles { .. }
                | Command::InstallJobScanMissingFiles { .. }
                | Command::InstallJobSkipMissing { .. }
                | Command::InstallJobRepairAndRetry { .. }) => {
                    downloads::execute(command).await
                }
                command @ (Command::JavaListVendors {}
                | Command::JavaListVersions { .. }
                | Command::JavaInstall { .. }
                | Command::JavaTest { .. }
                | Command::JavaSetDefault { .. }
                | Command::JavaRemoveDefault { .. }
                | Command::JavaRemoveRegistration { .. }) => {
                    java::execute(command).await
                }
                command @ (Command::WorldList { .. }
                | Command::WorldGet { .. }
                | Command::WorldRename { .. }
                | Command::WorldSetDisplayStatus { .. }
                | Command::WorldUpdateSettings { .. }
                | Command::WorldBackup { .. }
                | Command::WorldDelete { .. }
                | Command::InstanceLaunchWorld { .. }
                | Command::InstanceJoinServer { .. }
                | Command::ServerEntryList { .. }
                | Command::ServerEntryAdd { .. }
                | Command::ServerEntryUpdate { .. }
                | Command::ServerEntryRemove { .. }
                | Command::ServerPing { .. }
                | Command::InstanceGetProtocolVersion { .. }) => {
                    worlds::execute(command).await
                }
                Command::MetadataListGameVersions {
                    release_type,
                    search,
                    offset,
                    limit,
                } => metadata::games(release_type, search, offset, limit).await,
                Command::MetadataListLoaderVersions {
                    loader,
                    game_version,
                    stable_only,
                    offset,
                    limit,
                } => {
                    metadata::loaders(
                        loader,
                        game_version,
                        stable_only,
                        offset,
                        limit,
                    )
                    .await
                }
                command @ (Command::LaunchConfigGet { .. }
                | Command::LaunchSetMemory { .. }
                | Command::LaunchSetWindowOptions { .. }
                | Command::LaunchSetJava { .. }
                | Command::LaunchResetOverrides { .. }
                | Command::LaunchGetMemoryStatus { .. }) => {
                    launch::execute(command).await
                }
                command @ (Command::BackupRepositoryStatus {}
                | Command::BackupGetConfig { .. }
                | Command::BackupEnable { .. }
                | Command::BackupDisable { .. }
                | Command::BackupSetExclusions { .. }
                | Command::BackupCreate { .. }
                | Command::BackupListSnapshots { .. }
                | Command::BackupListOperations { .. }
                | Command::BackupGetOperation { .. }
                | Command::BackupCancel { .. }
                | Command::BackupPreviewRestore { .. }
                | Command::BackupGetRestorePreview { .. }
                | Command::BackupRestore { .. }
                | Command::BackupDeleteSnapshot { .. }) => {
                    backups::execute(self, command, context).await
                }
                command @ (Command::ResourceListSources {}
                | Command::ResourceSearch { .. }
                | Command::ResourceGet { .. }
                | Command::ResourceListReleases { .. }
                | Command::ResourceGetRelease { .. }
                | Command::ResourcePrepareInstall { .. }
                | Command::ResourceExecutePlan { .. }
                | Command::ResourceRevisePlan { .. }
                | Command::ContentRemove { .. }
                | Command::ContentList { .. }
                | Command::ContentSetEnabled { .. }) => {
                    resources::execute(self, command, context).await
                }
                Command::InstanceLaunch {
                    instance_id,
                    offline_mode,
                } => lifecycle::launch(instance_id, offline_mode).await,
                Command::ProcessGet { process_id } => {
                    lifecycle::get(process_id).await
                }
                Command::ProcessWait {
                    process_id,
                    timeout_seconds,
                } => lifecycle::wait(process_id, timeout_seconds).await,
                Command::ProcessKill { process_id } => {
                    lifecycle::kill(process_id).await
                }
                Command::InstallJobRetry { job_id } => Ok(jobs::summary(
                    theseus::install::retry_job(job_id)
                        .await
                        .map_err(business_error)?,
                )),
                Command::InstallJobResume { job_id } => Ok(jobs::summary(
                    theseus::install::resume_job(job_id)
                        .await
                        .map_err(business_error)?,
                )),
                Command::InstallJobCancel { job_id } => Ok(jobs::summary(
                    theseus::install::cancel_job(job_id)
                        .await
                        .map_err(business_error)?,
                )),
                Command::LogsList {
                    instance_id,
                    offset,
                    limit,
                } => logs::list(instance_id, offset, limit).await,
                Command::LogsRead {
                    instance_id,
                    log_type,
                    filename,
                    offset,
                    max_bytes,
                } => {
                    logs::read(
                        instance_id,
                        log_type,
                        filename,
                        offset,
                        max_bytes,
                    )
                    .await
                }
                Command::LogsSearch {
                    instance_id,
                    log_type,
                    filename,
                    query,
                    start_line,
                    limit,
                } => {
                    logs::search(
                        instance_id,
                        log_type,
                        filename,
                        query,
                        start_line,
                        limit,
                    )
                    .await
                }
                Command::InstallJobList {
                    include_finished,
                    offset,
                    limit,
                } => jobs::list(include_finished, offset, limit).await,
                Command::InstallJobGet { job_id } => jobs::get(job_id).await,
                Command::ProcessList { instance_id } => {
                    queries::processes(instance_id).await
                }
                Command::JavaList {} => queries::java().await,
                Command::SystemGetMemory {} => queries::memory().await,
                command => instances::execute(command).await,
            }
        })
    }
}

fn business_error(error: theseus::Error) -> ToolError {
    tracing::warn!(%error, "MCP launcher operation failed");
    ToolError::new("operation_failed", error.user_facing_message())
}

fn serialize(value: impl Serialize) -> Result<Value, ToolError> {
    serde_json::to_value(value).map_err(|_| {
        ToolError::new(
            "serialization_failed",
            "Unable to encode launcher result",
        )
    })
}

fn page<T: Serialize>(
    items: Vec<T>,
    offset: usize,
    limit: usize,
) -> Result<Value, ToolError> {
    let total = items.len();
    let items: Vec<_> = items.into_iter().skip(offset).take(limit).collect();
    let end = offset.saturating_add(items.len());
    Ok(
        json!({"items":items,"total":total,"next_offset": if end < total { Some(end) } else { None }}),
    )
}
