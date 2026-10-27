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
        context: BackendContext,
    ) -> BoxFuture<'_, Result<Value, ToolError>>;
}

/// Server-generated scope for prepared business objects, never a tool argument.
#[derive(Clone)]
pub struct BackendContext {
    pub session_id: String,
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
    ServerInstallForge {
        server_id: String,
        build: String,
        #[serde(default)]
        registered_java_path: Option<String>,
    },
    ServerInstallNeoforge {
        server_id: String,
        build: String,
        #[serde(default)]
        registered_java_path: Option<String>,
    },
    ServerInstallModpack {
        server_id: String,
        project_id: String,
        release_id: String,
        #[serde(default)]
        artifact_id: Option<String>,
    },
    ServerList {
        #[serde(default)]
        offset: usize,
        #[serde(default = "page_size")]
        limit: usize,
    },
    ServerGet {
        server_id: String,
    },
    ServerCreate {
        name: String,
        server_type: String,
        game_version: String,
        #[serde(default)]
        loader_version: Option<String>,
        #[serde(default)]
        registered_java_path: Option<String>,
        #[serde(default)]
        memory_mib: Option<u32>,
    },
    ServerUpdateSettings {
        server_id: String,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        registered_java_path: Option<String>,
        #[serde(default)]
        memory_mib: Option<u32>,
        #[serde(default)]
        pinned: Option<bool>,
    },
    ServerStart {
        server_id: String,
    },
    ServerWaitReady {
        server_id: String,
        #[serde(default = "wait_seconds")]
        timeout_seconds: u64,
    },
    ServerStop {
        server_id: String,
    },
    ServerKill {
        server_id: String,
    },
    ServerDelete {
        server_id: String,
    },
    ServerConsoleSend {
        server_id: String,
        command: String,
    },
    ServerLogsRead {
        server_id: String,
        #[serde(default)]
        offset: usize,
        #[serde(default = "text_bytes")]
        max_bytes: usize,
    },
    ServerLogsSearch {
        server_id: String,
        query: String,
        #[serde(default)]
        start_line: usize,
        #[serde(default = "page_size")]
        limit: usize,
    },
    AccountList {
        #[serde(default)]
        offline_only: bool,
        #[serde(default)]
        offset: usize,
        #[serde(default = "page_size")]
        limit: usize,
    },
    AccountGet {
        account_id: String,
    },
    AccountGetDefault {
        #[serde(default)]
        offline_mode: bool,
    },
    AccountSetDefault {
        account_id: String,
    },
    AccountRemove {
        account_id: String,
    },
    AccountAddOffline {
        username: String,
        #[serde(default)]
        uuid: Option<uuid::Uuid>,
    },
    AccountGetEligibility {},
    DownloadJobList {
        #[serde(default)]
        status: Option<String>,
        #[serde(default)]
        provider: Option<String>,
        #[serde(default)]
        query: Option<String>,
        #[serde(default)]
        offset: usize,
        #[serde(default = "page_size")]
        limit: usize,
    },
    DownloadJobGet {
        job_id: uuid::Uuid,
    },
    DownloadJobGetItems {
        job_id: uuid::Uuid,
        #[serde(default)]
        offset: usize,
        #[serde(default = "page_size")]
        limit: usize,
    },
    DownloadJobRetry {
        job_id: uuid::Uuid,
    },
    DownloadJobResume {
        job_id: uuid::Uuid,
    },
    DownloadJobCancel {
        job_id: uuid::Uuid,
    },
    DownloadJobDelete {
        job_id: uuid::Uuid,
    },
    DownloadHistoryClear {},
    InstallJobMissingFiles {
        job_id: uuid::Uuid,
        #[serde(default)]
        offset: usize,
        #[serde(default = "page_size")]
        limit: usize,
    },
    InstallJobScanMissingFiles {
        job_id: uuid::Uuid,
    },
    InstallJobSkipMissing {
        job_id: uuid::Uuid,
    },
    InstallJobRepairAndRetry {
        job_id: uuid::Uuid,
    },
    JavaListVendors {},
    JavaListVersions {
        vendor: String,
        #[serde(default)]
        offset: usize,
        #[serde(default = "page_size")]
        limit: usize,
    },
    JavaInstall {
        vendor: String,
        major_version: u32,
    },
    JavaTest {
        registered_path: String,
        major_version: u32,
    },
    JavaSetDefault {
        registered_path: String,
        major_version: u32,
    },
    JavaRemoveDefault {
        major_version: u32,
    },
    JavaRemoveRegistration {
        registered_path: String,
    },
    WorldList {
        instance_id: String,
        #[serde(default)]
        offset: usize,
        #[serde(default = "page_size")]
        limit: usize,
    },
    WorldGet {
        instance_id: String,
        world_id: String,
    },
    WorldRename {
        instance_id: String,
        world_id: String,
        name: String,
    },
    WorldSetDisplayStatus {
        instance_id: String,
        world_id: String,
        status: WorldDisplay,
    },
    WorldUpdateSettings {
        instance_id: String,
        world_id: String,
        #[serde(default)]
        game_mode: Option<GameMode>,
        #[serde(default)]
        difficulty: Option<Difficulty>,
        #[serde(default)]
        allow_commands: Option<bool>,
    },
    WorldBackup {
        instance_id: String,
        world_id: String,
    },
    WorldDelete {
        instance_id: String,
        world_id: String,
    },
    InstanceLaunchWorld {
        instance_id: String,
        world_id: String,
        #[serde(default)]
        offline_mode: bool,
    },
    InstanceJoinServer {
        instance_id: String,
        address: String,
        #[serde(default)]
        offline_mode: bool,
    },
    ServerEntryList {
        instance_id: String,
        #[serde(default)]
        offset: usize,
        #[serde(default = "page_size")]
        limit: usize,
    },
    ServerEntryAdd {
        instance_id: String,
        name: String,
        address: String,
        pack_status: PackPolicy,
    },
    ServerEntryUpdate {
        instance_id: String,
        index: usize,
        expected_name: String,
        expected_address: String,
        expected_pack_status: PackPolicy,
        name: String,
        address: String,
        pack_status: PackPolicy,
    },
    ServerEntryRemove {
        instance_id: String,
        index: usize,
        expected_name: String,
        expected_address: String,
        expected_pack_status: PackPolicy,
    },
    ServerPing {
        address: String,
        #[serde(default)]
        instance_id: Option<String>,
    },
    InstanceGetProtocolVersion {
        instance_id: String,
    },
    MetadataListGameVersions {
        #[serde(default)]
        release_type: Option<GameReleaseType>,
        #[serde(default)]
        search: Option<String>,
        #[serde(default)]
        offset: usize,
        #[serde(default = "page_size")]
        limit: usize,
    },
    MetadataListLoaderVersions {
        loader: MetadataLoader,
        game_version: String,
        #[serde(default)]
        stable_only: bool,
        #[serde(default)]
        offset: usize,
        #[serde(default = "page_size")]
        limit: usize,
    },
    LaunchConfigGet {
        instance_id: String,
    },
    LaunchSetMemory {
        instance_id: String,
        automatic: bool,
        maximum_mib: u32,
        optimize_before_launch: bool,
    },
    LaunchSetWindowOptions {
        instance_id: String,
        fullscreen: bool,
        maximized: bool,
        width: u16,
        height: u16,
    },
    LaunchSetJava {
        instance_id: String,
        registered_path: String,
    },
    LaunchResetOverrides {
        instance_id: String,
        fields: Vec<LaunchField>,
    },
    LaunchGetMemoryStatus {
        instance_id: String,
    },
    BackupRepositoryStatus {},
    BackupGetConfig {
        instance_id: String,
    },
    BackupEnable {
        instance_id: String,
    },
    BackupDisable {
        instance_id: String,
    },
    BackupSetExclusions {
        instance_id: String,
        files: Vec<String>,
        directories: Vec<String>,
    },
    BackupCreate {
        instance_id: String,
    },
    BackupListSnapshots {
        instance_id: String,
        #[serde(default)]
        offset: usize,
        #[serde(default = "page_size")]
        limit: usize,
    },
    BackupListOperations {
        instance_id: String,
        #[serde(default)]
        active_only: bool,
        #[serde(default)]
        offset: usize,
        #[serde(default = "page_size")]
        limit: usize,
    },
    BackupGetOperation {
        instance_id: String,
        backup_operation_id: uuid::Uuid,
    },
    BackupCancel {
        instance_id: String,
        backup_operation_id: uuid::Uuid,
    },
    BackupPreviewRestore {
        instance_id: String,
        snapshot_id: String,
    },
    BackupGetRestorePreview {
        preview_id: uuid::Uuid,
    },
    BackupRestore {
        preview_id: uuid::Uuid,
    },
    BackupDeleteSnapshot {
        instance_id: String,
        snapshot_id: String,
    },
    ResourceListSources {},
    ResourceSearch {
        source: Source,
        query: String,
        resource_type: ResourceKind,
        #[serde(default)]
        instance_id: Option<String>,
        #[serde(default)]
        cursor: Option<String>,
        #[serde(default = "search_size")]
        limit: u32,
    },
    ResourceGet {
        source: Source,
        project_id: String,
    },
    ResourceListReleases {
        source: Source,
        project_id: String,
        instance_id: String,
        #[serde(default)]
        cursor: Option<String>,
    },
    ResourceGetRelease {
        source: Source,
        project_id: String,
        release_id: String,
    },
    ResourcePrepareInstall {
        instance_id: String,
        source: Source,
        project_id: String,
        release_id: String,
        resource_type: InstallKind,
    },
    ResourceExecutePlan {
        plan_id: uuid::Uuid,
        revision: u32,
    },
    ResourceRevisePlan {
        plan_id: uuid::Uuid,
        revision: u32,
        #[serde(default)]
        ignore_dependencies: bool,
        #[serde(default)]
        accept_compatibility_nodes: Vec<String>,
    },
    ContentRemove {
        instance_id: String,
        entry_id: String,
    },
    ContentList {
        instance_id: String,
        #[serde(default)]
        offset: usize,
        #[serde(default = "page_size")]
        limit: usize,
    },
    ContentSetEnabled {
        instance_id: String,
        entry_id: String,
        enabled: bool,
    },
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

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GameReleaseType {
    Release,
    Snapshot,
    OldAlpha,
    OldBeta,
}
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetadataLoader {
    Fabric,
    Forge,
    Quilt,
    #[serde(rename = "neoforge")]
    NeoForge,
    Optifine,
    Cleanroom,
    LiteLoader,
    LegacyFabric,
    Babric,
}
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaunchField {
    Memory,
    Window,
    Java,
}

impl Command {
    pub(crate) fn deferred(&self) -> bool {
        matches!(
            self,
            Self::InstanceLaunch { .. }
                | Self::InstallJobRetry { .. }
                | Self::InstallJobResume { .. }
                | Self::InstallJobCancel { .. }
                | Self::ResourcePrepareInstall { .. }
                | Self::ResourceExecutePlan { .. }
                | Self::ResourceRevisePlan { .. }
                | Self::ContentRemove { .. }
                | Self::BackupEnable { .. }
                | Self::BackupDisable { .. }
                | Self::BackupSetExclusions { .. }
                | Self::BackupCreate { .. }
                | Self::BackupPreviewRestore { .. }
                | Self::BackupRestore { .. }
                | Self::BackupDeleteSnapshot { .. }
                | Self::WorldBackup { .. }
                | Self::WorldDelete { .. }
                | Self::InstanceLaunchWorld { .. }
                | Self::InstanceJoinServer { .. }
                | Self::JavaInstall { .. }
                | Self::JavaTest { .. }
                | Self::JavaSetDefault { .. }
                | Self::DownloadJobRetry { .. }
                | Self::DownloadJobResume { .. }
                | Self::DownloadJobCancel { .. }
                | Self::DownloadJobDelete { .. }
                | Self::DownloadHistoryClear { .. }
                | Self::InstallJobScanMissingFiles { .. }
                | Self::InstallJobSkipMissing { .. }
                | Self::InstallJobRepairAndRetry { .. }
                | Self::AccountSetDefault { .. }
                | Self::AccountRemove { .. }
                | Self::AccountAddOffline { .. }
                | Self::ServerCreate { .. }
                | Self::ServerStart { .. }
                | Self::ServerDelete { .. }
                | Self::ServerConsoleSend { .. }
                | Self::ServerInstallForge { .. }
                | Self::ServerInstallNeoforge { .. }
                | Self::ServerInstallModpack { .. }
        )
    }

    pub(crate) fn interrupts_mutation(&self) -> bool {
        matches!(self, Self::ServerStop { .. } | Self::ServerKill { .. })
    }
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorldDisplay {
    Normal,
    Hidden,
    Favorite,
}
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GameMode {
    Survival,
    Creative,
    Adventure,
    Spectator,
}
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Difficulty {
    Peaceful,
    Easy,
    Normal,
    Hard,
}
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackPolicy {
    Enabled,
    Disabled,
    Prompt,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Modrinth,
    Curseforge,
    Mcarchive,
    PlanetMinecraft,
}
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceKind {
    Mod,
    Plugin,
    DataPack,
    ResourcePack,
    Shader,
    ModPack,
    World,
}
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallKind {
    Mod,
    ResourcePack,
    Shader,
}
fn search_size() -> u32 {
    20
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
