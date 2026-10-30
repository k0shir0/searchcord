//! Writable collection storage. Network and web frameworks are separate boundaries.
pub mod storage;
pub use storage::{PageContext, Result, Store};
pub mod credentials;
