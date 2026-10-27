use crate::{Command, ToolError, session::Shared};
use serde_json::{Value, json};
use std::{collections::VecDeque, sync::Arc};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
pub(crate) struct Operations(VecDeque<Operation>);

struct Operation {
    id: String,
    transport: String,
    control: String,
    tool: String,
    status: &'static str,
    result: Option<Value>,
}

impl Operation {
    fn value(&self) -> Value {
        json!({"operation_id":self.id,"tool":self.tool,"status":self.status,"result":self.result})
    }
}

impl Shared {
    pub(crate) fn operation_get(
        &self,
        transport: &str,
        control: &str,
        id: &str,
    ) -> Result<Value, ToolError> {
        self.authorize(transport, control, false)?;
        self.operations
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .0
            .iter()
            .find(|op| {
                op.id == id
                    && op.transport == transport
                    && op.control == control
            })
            .map(Operation::value)
            .ok_or_else(|| {
                ToolError::new(
                    "not_found",
                    "Operation not found in this control session",
                )
            })
    }

    pub(crate) fn operation_list(
        &self,
        transport: &str,
        control: &str,
    ) -> Result<Value, ToolError> {
        self.authorize(transport, control, false)?;
        Ok(
            json!({"items":self.operations.lock().unwrap_or_else(|e| e.into_inner()).0.iter().rev()
            .filter(|op| op.transport == transport && op.control == control).map(Operation::value).collect::<Vec<_>>()}),
        )
    }

    pub(crate) fn enqueue(
        self: &Arc<Self>,
        transport: &str,
        control: &str,
        entry: &crate::catalog::Entry,
        command: Command,
        request_cancel: CancellationToken,
    ) -> Result<Value, ToolError> {
        if request_cancel.is_cancelled() {
            return Err(ToolError::new(
                "cancelled",
                "Request cancelled before acceptance",
            ));
        }
        let full = entry.access == crate::catalog::Access::FullAccess;
        let cancel = self.authorize(transport, control, full)?;
        let id = uuid::Uuid::new_v4().to_string();
        {
            let mut ops =
                self.operations.lock().unwrap_or_else(|e| e.into_inner());
            if ops.0.len() >= 64 {
                let oldest = ops
                    .0
                    .iter()
                    .position(|op| !matches!(op.status, "queued" | "running"))
                    .ok_or_else(|| {
                        ToolError::new(
                            "too_many_operations",
                            "Wait for current operations to finish",
                        )
                    })?;
                ops.0.remove(oldest);
            }
            ops.0.push_back(Operation {
                id: id.clone(),
                transport: transport.into(),
                control: control.into(),
                tool: entry.tool.name.to_string(),
                status: "queued",
                result: None,
            });
        }
        let shared = self.clone();
        let transport = transport.to_owned();
        let control = control.to_owned();
        let operation_id = id.clone();
        tokio::spawn(async move {
            let result = async {
                let _permit = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return Err(ToolError::new("session_revoked", "Control session ended before execution")),
                    permit = shared.mutations.lock() => permit,
                };
                shared.authorize(&transport, &control, full)?;
                shared.operation_update(&operation_id, "running", None);
                // Catch backend panics too: an accepted operation must not remain
                // permanently running if a business implementation panics.
                let backend = shared.backend.clone();
                let context = crate::BackendContext { session_id:control.clone() };
                tokio::spawn(async move { backend.execute(command, context).await }).await
                    .map_err(|_| ToolError::new("execution_failed", "Operation task failed; inspect launcher state before retrying"))?
            }.await;
            match result {
                Ok(value) => shared.operation_update(
                    &operation_id,
                    "succeeded",
                    Some(value),
                ),
                Err(error) => shared.operation_update(
                    &operation_id,
                    if matches!(
                        error.code,
                        "session_revoked" | "session_required" | "cancelled"
                    ) {
                        "cancelled"
                    } else {
                        "failed"
                    },
                    Some(json!({"error":error})),
                ),
            }
        });
        Ok(json!({"operation_id":id,"status":"queued"}))
    }

    fn operation_update(
        &self,
        id: &str,
        status: &'static str,
        result: Option<Value>,
    ) {
        if let Some(op) = self
            .operations
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .0
            .iter_mut()
            .find(|op| op.id == id)
        {
            op.status = status;
            op.result = result;
        }
    }
}
