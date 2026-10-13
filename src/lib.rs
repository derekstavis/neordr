//! Herdr-compatible session transport backed by Neovim's public RPC interface.
//! Neovim owns the UI and terminal jobs; Moshi owns transcript interpretation.

#[cfg(not(unix))]
compile_error!("Neordr currently supports macOS and Linux Unix sockets only");

mod activity;
mod agent_sessions;
pub mod api;
pub mod attach;
pub mod backend;
pub mod contracts;
pub mod editors;
pub mod endpoint;
pub mod gestures;
pub mod nvim;
pub mod ordering;
mod parallel;
pub mod process;
pub mod protocol;
pub mod router;
pub mod selection;
pub mod server;
pub mod session;

pub mod control;
pub mod doctor;
