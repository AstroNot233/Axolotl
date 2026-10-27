//! Local MCP transport and launcher session lifecycle.
mod backend;
mod catalog;
mod control;
mod http;
mod info;
pub mod log_text;
mod operations;
mod protocol;
mod scoped;
mod server;
mod session;
mod tools;
pub mod world_input;

pub use backend::{
    Backend, BackendContext, Command, InstallKind, Loader, LogKind,
    ResourceKind, Source, ToolError,
};
pub use backend::{Difficulty, GameMode, PackPolicy, WorldDisplay};
pub use backend::{GameReleaseType, LaunchField, MetadataLoader};
pub use info::LauncherInfo;
pub use scoped::ScopedRegistry;
pub use server::Server;
pub use session::{Connection, Snapshot};

#[cfg(test)]
mod tests;
