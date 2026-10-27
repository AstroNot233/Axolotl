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
    InstanceLaunch {
        instance_id: String,
        #[serde(default)]
        offline_mode: bool,
    },
    ProcessGet {
        process_id: uuid::Uuid,
    },
    ProcessWait {
        process_id: uuid::Uuid,
        #[serde(default = "wait_seconds")]
        timeout_seconds: u64,
    },
    ProcessKill {
        process_id: uuid::Uuid,
    },
    InstallJobRetry {
        job_id: uuid::Uuid,
    },
    InstallJobResume {
        job_id: uuid::Uuid,
    },
    InstallJobCancel {
        job_id: uuid::Uuid,
    },
    LogsList {
        instance_id: String,
        #[serde(default)]
        offset: usize,
        #[serde(default = "page_size")]
        limit: usize,
    },
    LogsRead {
        instance_id: String,
        log_type: LogKind,
        filename: String,
        #[serde(default)]
        offset: usize,
        #[serde(default = "text_bytes")]
        max_bytes: usize,
    },
    LogsSearch {
        instance_id: String,
        log_type: LogKind,
        filename: String,
        query: String,
        #[serde(default)]
        start_line: usize,
        #[serde(default = "page_size")]
        limit: usize,
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

impl Command {
    pub(crate) fn deferred(&self) -> bool {
        matches!(
            self,
            Self::InstanceLaunch { .. }
                | Self::InstallJobRetry { .. }
                | Self::InstallJobResume { .. }
                | Self::InstallJobCancel { .. }
        )
    }
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogKind {
    Info,
    Crash,
    Jvm,
    Launcher,
}

fn wait_seconds() -> u64 {
    10
}
fn text_bytes() -> usize {
    16 * 1024
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
