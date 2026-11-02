//! Writable collection storage. Network and web frameworks are separate boundaries.
pub mod storage;
pub use storage::{PageContext, Result, Store};
pub mod accounts;
pub mod analytics;
pub mod credentials;
pub mod discord;
pub mod discovery;
pub mod dm_cleanup;
#[cfg(windows)]
mod dpapi;
pub mod invites;
pub mod media_collection;
pub mod migrations;
pub mod monitor;
pub mod pacing;
pub mod profiles;
pub mod recovery;
pub mod scrapes;
pub mod writer;
