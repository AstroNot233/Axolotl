//! Local MCP transport and launcher session lifecycle.
mod backend;
mod catalog;
mod control;
mod http;
mod info;
mod protocol;
mod server;
mod session;
mod tools;

pub use backend::{Backend, Command, Loader, ToolError};
pub use info::LauncherInfo;
pub use server::Server;
pub use session::{Connection, Snapshot};

#[cfg(test)]
mod tests;
