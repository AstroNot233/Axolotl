use launcher_mcp::ToolError;
use serde::Serialize;
use serde_json::{Value, json};
use std::{collections::HashMap, sync::Mutex};
use tauri::{Emitter, Manager};
use tokio::sync::oneshot;
use uuid::Uuid;

pub(super) const EVENT: &str = "mcp-ui-request";
#[derive(Default)]
pub(super) struct UiBridge(Mutex<HashMap<Uuid, Pending>>);
struct Pending {
    control: String,
    full: bool,
    action: Option<Value>,
    answer: oneshot::Sender<Result<Value, ToolError>>,
}
struct PendingGuard {
    app: tauri::AppHandle,
    id: Uuid,
}
impl Drop for PendingGuard {
    fn drop(&mut self) {
        self.app
            .state::<UiBridge>()
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.id);
    }
}

async fn active(app: &tauri::AppHandle, control: &str, full: bool) -> bool {
    let service = app.state::<super::service::McpService>();
    let controller = service.0.lock().await;
    controller
        .server
        .as_ref()
        .is_some_and(|server| server.control_is_active(control, full))
}

pub(super) async fn request(
    app: &tauri::AppHandle,
    control: String,
    full: bool,
    action: Value,
) -> Result<Value, ToolError> {
    if !active(app, &control, full).await {
        return Err(ToolError::new("session_revoked", "Control session ended"));
    }
    let id = Uuid::new_v4();
    let (send, receive) = oneshot::channel();
    {
        let bridge = app.state::<UiBridge>();
        let mut pending = bridge.0.lock().unwrap_or_else(|e| e.into_inner());
        if pending.len() >= 8 {
            return Err(ToolError::new(
                "ui_busy",
                "Too many pending UI requests",
            ));
        }
        pending.insert(
            id,
            Pending {
                control,
                full,
                action: Some(action),
                answer: send,
            },
        );
    }
    let _guard = PendingGuard {
        app: app.clone(),
        id,
    };
    app.emit_to("main", EVENT, json!({"request_id":id}))
        .map_err(|_| {
            ToolError::new("ui_unavailable", "Main launcher UI is unavailable")
        })?;
    tokio::time::timeout(std::time::Duration::from_secs(15), receive)
        .await
        .map_err(|_| {
            ToolError::new(
                "ui_timeout",
                "Launcher UI did not respond; observe state before retrying",
            )
        })?
        .map_err(|_| ToolError::new("ui_cancelled", "UI request cancelled"))?
}

fn main_window(window: &tauri::WebviewWindow) -> Result<(), String> {
    if window.label() != "main" {
        return Err(
            "Only the main launcher webview may answer UI requests".into()
        );
    }
    Ok(())
}

#[derive(Serialize)]
pub(super) struct UiClaim {
    action: Value,
    scope: String,
}

#[tauri::command]
pub(super) async fn mcp_ui_claim(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    request_id: Uuid,
) -> Result<UiClaim, String> {
    main_window(&window)?;
    let (control, full) = {
        let bridge = app.state::<UiBridge>();
        let pending = bridge.0.lock().unwrap_or_else(|e| e.into_inner());
        let pending = pending.get(&request_id).ok_or("UI request expired")?;
        (pending.control.clone(), pending.full)
    };
    if !active(&app, &control, full).await {
        return Err("Control session ended".into());
    }
    let bridge = app.state::<UiBridge>();
    let mut pending = bridge.0.lock().unwrap_or_else(|e| e.into_inner());
    let action = pending
        .get_mut(&request_id)
        .and_then(|p| p.action.take())
        .ok_or("UI request expired or already claimed")?;
    Ok(UiClaim {
        action,
        scope: control,
    })
}

#[tauri::command]
pub(super) async fn mcp_ui_respond(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    request_id: Uuid,
    result: Value,
) -> Result<(), String> {
    main_window(&window)?;
    let (control, full) = {
        let bridge = app.state::<UiBridge>();
        let pending = bridge.0.lock().unwrap_or_else(|e| e.into_inner());
        let pending = pending
            .get(&request_id)
            .filter(|p| p.action.is_none())
            .ok_or("UI request not claimed or expired")?;
        (pending.control.clone(), pending.full)
    };
    let allowed = active(&app, &control, full).await;
    let encoded =
        serde_json::to_vec(&result).map_err(|_| "Invalid UI result")?;
    if encoded.len() > 8 * 1024 * 1024 {
        return Err("UI result exceeds size limit".into());
    }
    let answer = if !allowed {
        Err(ToolError::new("session_revoked", "Control session ended"))
    } else if result["ok"] == true {
        Ok(result["data"].clone())
    } else {
        Err(ToolError::new(
            "ui_action_failed",
            result["error"]
                .as_str()
                .unwrap_or("UI action failed")
                .chars()
                .take(256)
                .collect::<String>(),
        ))
    };
    let bridge = app.state::<UiBridge>();
    if let Some(pending) = bridge
        .0
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&request_id)
    {
        let _ = pending.answer.send(answer);
    }
    Ok(())
}
