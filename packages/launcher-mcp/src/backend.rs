//! Typed boundary between the MCP protocol and the launcher's business APIs.
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Implementations must finish an admitted mutation even if its caller disconnects.
/// Reads may be dropped on cancellation. Never expose arbitrary native IPC here.
pub trait Backend: Send + Sync + 'static {
    fn execute(
        &self,
        command: Command,
    ) -> BoxFuture<'_, Result<Value, ToolError>>;
}

#[derive(Debug, Serialize)]
pub struct ToolError {
    pub code: &'static str,
    pub message: String,
}

impl ToolError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(
    tag = "name",
    content = "arguments",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum Command {
    InstanceList {
        #[serde(default)]
        offset: usize,
        #[serde(default = "page_size")]
        limit: usize,
        #[serde(default)]
        search: Option<String>,
    },
    InstanceGet {
        instance_id: String,
    },
    InstanceCreate {
        name: String,
        game_version: String,
        loader: Loader,
        #[serde(default)]
        loader_version: Option<String>,
    },
    InstanceRename {
        instance_id: String,
        name: String,
    },
    InstanceSetPinned {
        instance_id: String,
        pinned: bool,
    },
    InstanceGroupList {},
    InstanceGroupCreate {
        name: String,
    },
    InstanceGroupRename {
        group_id: String,
        name: String,
    },
    InstanceGroupDelete {
        group_id: String,
    },
    InstanceSetGroups {
        instance_id: String,
        group_ids: Vec<String>,
    },
    ProcessList {
        #[serde(default)]
        instance_id: Option<String>,
    },
    JavaList {},
    SystemGetMemory {},
    InstallJobList {
        #[serde(default)]
        include_finished: bool,
        #[serde(default)]
        offset: usize,
        #[serde(default = "page_size")]
        limit: usize,
    },
    InstallJobGet {
        job_id: uuid::Uuid,
    },
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Loader {
    Vanilla,
    Fabric,
    Forge,
    Quilt,
    #[serde(rename = "neoforge")]
    NeoForge,
}

fn page_size() -> usize {
    50
}
