//! The checks of `ds doctor`: each one turns facts about the checkout into findings.

pub mod compose;
pub mod env;
pub mod env_config;
pub mod finding;
pub mod isolation;
pub mod local_host;
pub mod session;
pub mod slot;
pub mod vite;
pub mod vite_ports;
pub mod web_port;
pub mod worktree_guard;
