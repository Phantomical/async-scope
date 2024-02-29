//! A shim module that either contains macros from [`tracing`] or shim macros
//! that do nothing, depending on whether the `tracing` feature is enabled.

#[cfg(not(feature = "tracing"))]
macro_rules! trace {
    ($($tt:tt)*) => {};
}

#[cfg(not(feature = "tracing"))]
pub(crate) use trace;
#[cfg(feature = "tracing")]
pub(crate) use tracing::trace;
