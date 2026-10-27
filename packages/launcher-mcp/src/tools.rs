use crate::{
    Command, ToolError,
    catalog::{self, Access},
    session::Shared,
};
use rmcp::{
    ErrorData, RoleServer,
    model::{CallToolRequestParams, CallToolResult},
    service::RequestContext,
};
use serde_json::{Value, json};
use std::sync::Arc;

pub(crate) async fn call(
    shared: &Arc<Shared>,
    transport: &str,
    request: CallToolRequestParams,
    context: RequestContext<RoleServer>,
) -> Result<CallToolResult, ErrorData> {
    let mut arguments = request.arguments.unwrap_or_default();
    let entry = catalog::entries()
        .into_iter()
        .find(|entry| entry.tool.name == request.name)
        .ok_or_else(|| {
            ErrorData::invalid_params("Unknown launcher tool", None)
        })?;
    catalog::validate(&entry, &arguments)?;
    let result = match request.name.as_ref() {
        "get_launcher_info" => {
            Ok(serde_json::to_value(&shared.info).expect("launcher metadata"))
        }
        "get_launcher_capabilities" => Ok(json!({
            "tools": catalog::entries().into_iter().map(|entry| json!({
                "name": entry.tool.name,
                "requires_session": entry.access != Access::Public,
                "requires_full_access": entry.access == Access::FullAccess,
                "read_only": entry.read_only,
                "returns_operation_id": matches!(entry.tool.name.as_ref(), "instance_launch" | "install_job_retry" | "install_job_resume" | "install_job_cancel" | "resource_prepare_install" | "resource_execute_plan" | "resource_revise_plan" | "content_remove" | "backup_enable" | "backup_disable" | "backup_set_exclusions" | "backup_create" | "backup_preview_restore" | "backup_restore" | "backup_delete_snapshot" | "world_backup" | "world_delete" | "instance_launch_world" | "instance_join_server" | "java_install" | "java_test" | "java_set_default" | "download_job_retry" | "download_job_resume" | "download_job_cancel" | "download_job_delete" | "download_history_clear" | "install_job_scan_missing_files" | "install_job_skip_missing" | "install_job_repair_and_retry"),
            })).collect::<Vec<_>>(),
            "restrictions": {
                "ordinary_mode": {"manual_control_approval": true, "deletion": false, "direct_interface_control": false},
                "session_scoped_to_transport": true,
                "max_page_size": 100,
                "in_flight_mutations_finish_on_disconnect": true,
                "created_installation_jobs_continue_after_session_end": true,
                "running_games_continue_after_session_end": true,
                "retained_operation_limit": 64,
                "prepared_plan_ttl_seconds": 600,
                "prepared_plan_limit": 64,
            }
        })),
        "start_session" => shared
            .start_control(transport, context)
            .await
            .map_err(|error| ToolError::new("control_not_granted", error)),
        "end_session" => shared
            .release_control(
                transport,
                arguments["session_id"].as_str().unwrap(),
            )
            .map(|()| json!({"ended":true}))
            .map_err(|error| ToolError::new("invalid_session", error)),
        "operations_get" => shared.operation_get(
            transport,
            arguments["session_id"].as_str().unwrap(),
            arguments["operation_id"].as_str().unwrap(),
        ),
        "operations_list" => shared.operation_list(
            transport,
            arguments["session_id"].as_str().unwrap(),
        ),
        _ => {
            let control = arguments
                .remove("session_id")
                .unwrap()
                .as_str()
                .unwrap()
                .to_owned();
            let command = serde_json::from_value::<Command>(
                json!({"name":request.name,"arguments":arguments}),
            )
            .map_err(|error| {
                ErrorData::invalid_params(error.to_string(), None)
            })?;
            if command.deferred() {
                shared.enqueue(
                    transport,
                    &control,
                    &entry,
                    command,
                    context.ct.clone(),
                )
            } else {
                execute(shared, transport, &control, &entry, command, context)
                    .await
            }
        }
    };
    Ok(match result {
        Ok(value) => CallToolResult::structured(value),
        Err(error) => CallToolResult::structured_error(json!({"error":error})),
    })
}

async fn execute(
    shared: &Arc<Shared>,
    transport: &str,
    control: &str,
    entry: &catalog::Entry,
    command: Command,
    context: RequestContext<RoleServer>,
) -> Result<Value, ToolError> {
    let full = entry.access == Access::FullAccess;
    let cancel = shared.authorize(transport, control, full)?;
    let backend_context = crate::BackendContext {
        session_id: control.into(),
    };
    if entry.read_only {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(ToolError::new("session_revoked", "Control session ended")),
            _ = context.ct.cancelled() => Err(ToolError::new("cancelled", "Request cancelled")),
            result = shared.backend.execute(command, backend_context) => result,
        }
    } else {
        let shared = shared.clone();
        let transport = transport.to_owned();
        let control = control.to_owned();
        // Drain admitted writes if rmcp drops the handler on cancellation. Never
        // drop business futures halfway through a filesystem/database mutation.
        tokio::spawn(async move {
            let _permit = tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(ToolError::new("session_revoked", "Control session ended before execution")),
                _ = context.ct.cancelled() => return Err(ToolError::new("cancelled", "Request cancelled before execution")),
                permit = shared.mutations.lock() => permit,
            };
            shared.authorize(&transport, &control, full)?;
            if context.ct.is_cancelled() {
                return Err(ToolError::new("cancelled", "Request cancelled before execution"));
            }
            shared.backend.execute(command, backend_context).await
        }).await.map_err(|_| ToolError::new("execution_failed", "Launcher operation task failed; inspect current state before retrying"))?
    }
}
