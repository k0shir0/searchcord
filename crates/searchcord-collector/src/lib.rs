//! Writable collection storage. Network and web frameworks are separate boundaries.
pub mod storage;
pub use storage::{PageContext, Result, Store};
pub mod credentials;
pub mod discord;
pub mod discovery;
#[cfg(windows)]
mod dpapi;
pub mod migrations;
pub mod pacing;
pub mod recovery;
pub mod profiles;
