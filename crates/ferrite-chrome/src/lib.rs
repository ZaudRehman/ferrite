//! Chrome contract: token packs plus typed IPC. Bundled assets only.
//! No remote URLs, no eval. Scripts arrive via the typed IPC handler.

pub mod chrome;
pub mod commands;
pub mod ipc;
pub mod tokens;
