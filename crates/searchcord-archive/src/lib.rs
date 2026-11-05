//! Credential-free archive operations shared by native tools and HTTP adapters.
mod archive;
pub mod codec;
pub mod search;
pub use archive::{Archive, ArchiveError, ReadSession, SearchOptions, snowflake_day};
pub mod media;
mod presentation;
pub use presentation::{image_url, search_label, unicode_digits, user_id};
