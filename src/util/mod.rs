//! Utilities used by the rest of the crate.
//!
//! These are types that aren't directly part of implementing the API but are
//! instead used as internal helpers.

pub mod cell;
pub mod split_arc;
mod sync;

pub(crate) use self::cell::OneshotCell;
pub(crate) use self::sync::SyncWrapper;
