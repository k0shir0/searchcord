//! Credential-free archive operations shared by native tools and HTTP adapters.
mod archive;
pub mod codec;
pub mod search;
pub use archive::{Archive, ArchiveError, ReadSession, SearchOptions, snowflake_day};
