mod instances;
mod jobs;
mod lifecycle;
mod logs;
mod queries;

use futures::future::BoxFuture;
use launcher_mcp::{Backend, Command, ToolError};
use serde::Serialize;
use serde_json::{Value, json};

pub(super) struct LauncherBackend;

impl Backend for LauncherBackend {
    fn execute(
        &self,
        command: Command,
    ) -> BoxFuture<'_, Result<Value, ToolError>> {
        Box::pin(async move {
            match command {
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
