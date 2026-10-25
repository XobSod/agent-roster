//! End-to-end tests of the CLI against a real tmux server on a private socket,
//! with a stand-in script as the agent and `roster report` called the way Claude's
//! hooks would call it.

#[path = "../common/mod.rs"]
mod common;

mod claude;
mod codex;
mod config;
mod generic;
mod pi;
mod sessions;
mod usage;
