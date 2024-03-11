//! Utilities used by the rest of the crate.
//!
//! These are types that aren't directly part of implementing the API but are
//! instead used as internal helpers.

pub mod complete;
mod sync;
#[cfg(test)]
pub mod test;
mod uncontended;
pub mod variance;

pub(crate) use self::sync::SyncWrapper;
pub(crate) use self::uncontended::Uncontended;
