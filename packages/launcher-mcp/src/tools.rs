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
                "returns_operation_id": matches!(entry.tool.name.as_ref(), "core_preview_jar" | "core_add_jar_mod" | "core_replace_jar" | "core_set_enabled" | "core_move_component" | "core_remove_component" | "core_restore_component" | "core_install_mcarchive_loader" | "core_import_mcarchive_loader" | "multiplayer_list_nodes" | "multiplayer_set_provider" | "multiplayer_prepare_provider" | "multiplayer_host" | "multiplayer_join" | "multiplayer_stop" | "multiplayer_reset" | "multiplayer_check_update" | "multiplayer_update" | "multiplayer_logs_read" | "multiplayer_get_diagnostics" | "screenshot_save_edit" | "screenshot_delete" | "screenshot_move" | "screenshot_export" | "screenshot_group_create" | "screenshot_group_rename" | "screenshot_group_delete" | "screenshot_set_group" | "skin_list" | "cape_list" | "skin_get_texture" | "skin_import" | "skin_save" | "skin_equip" | "skin_delete" | "skin_unequip" | "skin_set_order" | "cape_equip" | "cape_unequip" | "file_write" | "file_apply_patch" | "file_create_directory" | "file_copy" | "file_move" | "file_trash" | "archive_create" | "archive_extract" | "nbt_patch" | "resource_check_updates" | "resource_prepare_update" | "resource_prepare_switch" | "resource_execute_changes" | "modpack_prepare_install" | "modpack_prepare_local" | "modpack_execute_install" | "instance_scan_imports" | "instance_prepare_import" | "instance_execute_import" | "instance_plan_upgrade" | "instance_get_upgrade_plan" | "instance_revise_upgrade" | "instance_select_upgrade_solution" | "instance_execute_upgrade" | "instance_launch" | "install_job_retry" | "install_job_resume" | "install_job_cancel" | "resource_prepare_install" | "resource_execute_plan" | "resource_revise_plan" | "content_remove" | "backup_enable" | "backup_disable" | "backup_set_exclusions" | "backup_create" | "backup_preview_restore" | "backup_restore" | "backup_delete_snapshot" | "world_backup" | "world_delete" | "instance_launch_world" | "instance_join_server" | "java_install" | "java_test" | "java_set_default" | "download_job_retry" | "download_job_resume" | "download_job_cancel" | "download_job_delete" | "download_history_clear" | "install_job_scan_missing_files" | "install_job_skip_missing" | "install_job_repair_and_retry" | "account_set_default" | "account_remove" | "account_add_offline" | "server_create" | "server_start" | "server_delete" | "server_console_send" | "server_install_forge" | "server_install_neoforge" | "server_install_modpack" | "instance_duplicate" | "instance_reinstall" | "instance_delete" | "resource_favorite_add" | "resource_favorite_remove" | "resource_history_rollback" | "resource_scan_backups" | "resource_prepare_history_cleanup" | "resource_execute_history_cleanup"),
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
    let result = if matches!(
        request.name.as_ref(),
        "ui_screenshot" | "screenshot_get"
    ) {
        result.and_then(screenshot)
    } else if request.name == "operations_get" {
        result.and_then(|mut value| {
            if value["status"] == "succeeded"
                && value["result"]["image"].is_object()
            {
                let mut image = screenshot(value["result"].take())?;
                value["result"] =
                    image.structured_content.take().unwrap_or(Value::Null);
                image.structured_content = Some(value);
                Ok(image)
            } else {
                Ok(CallToolResult::structured(value))
            }
        })
    } else {
        result.map(CallToolResult::structured)
    };
    Ok(match result {
        Ok(value) => value,
        Err(error) => CallToolResult::structured_error(json!({"error":error})),
    })
}

fn screenshot(mut value: Value) -> Result<CallToolResult, ToolError> {
    let image = value.get_mut("image").map(Value::take).ok_or_else(|| {
        ToolError::new(
            "invalid_screenshot",
            "Launcher did not return a screenshot",
        )
    })?;
    let data = image["data"]
        .as_str()
        .filter(|data| {
            data.starts_with("iVBORw0KGgo") && data.len() <= 7 * 1024 * 1024
        })
        .filter(|_| image["mime_type"] == "image/png")
        .ok_or_else(|| {
            ToolError::new(
                "invalid_screenshot",
                "Invalid or oversized launcher PNG",
            )
        })?;
    let mut result = CallToolResult::success(vec![
        rmcp::model::ContentBlock::image(data, "image/png"),
        rmcp::model::ContentBlock::text(value["context"].to_string()),
    ]);
    result.structured_content = Some(json!({"context":value["context"]}));
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn screenshots_are_image_content_without_duplicate_base64_metadata() {
        let result = screenshot(json!({"image":{"data":"iVBORw0KGgoAAA","mime_type":"image/png"},"context":{"view_id":"view"}})).unwrap();
        let value = serde_json::to_value(result).unwrap();
        assert_eq!(value["content"][0]["type"], "image");
        assert_eq!(value["content"][0]["mimeType"], "image/png");
        assert_eq!(value["structuredContent"]["context"]["view_id"], "view");
        assert!(!value["structuredContent"].to_string().contains("iVBOR"));
        assert!(screenshot(json!({"image":{"data":"javascript:bad","mime_type":"image/png"}})).is_err());
    }
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
            let _permit = if command.interrupts_mutation() { None } else {Some(tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(ToolError::new("session_revoked", "Control session ended before execution")),
                _ = context.ct.cancelled() => return Err(ToolError::new("cancelled", "Request cancelled before execution")),
                permit = shared.mutations.lock() => permit,
            })};
            shared.authorize(&transport, &control, full)?;
            if context.ct.is_cancelled() {
                return Err(ToolError::new("cancelled", "Request cancelled before execution"));
            }
            shared.backend.execute(command, backend_context).await
        }).await.map_err(|_| ToolError::new("execution_failed", "Launcher operation task failed; inspect current state before retrying"))?
    }
}
