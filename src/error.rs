use std::any::Any;
use std::fmt;

use crate::util::SyncWrapper;

pub(crate) type Payload = Box<dyn Any + Send + 'static>;

/// A task failed to execute to completion.
pub struct JoinError(Detail);

enum Detail {
    Cancelled,
    Panic(SyncWrapper<Payload>),
}

impl JoinError {
    pub(crate) fn cancelled() -> Self {
        Self(Detail::Cancelled)
    }

    pub(crate) fn panicked(panic: Payload) -> Self {
        Self(Detail::Panic(SyncWrapper::new(panic)))
    }

    /// Returns true if the error was caused by the task being cancelled.
    pub fn is_cancelled(&self) -> bool {
        matches!(self.0, Detail::Cancelled)
    }

    /// Returns true if the error was caused by the task panicking.
    pub fn is_panic(&self) -> bool {
        matches!(self.0, Detail::Panic(_))
    }

    /// Consume this error, returning the object with which the task panicked.
    ///
    /// # Panics
    /// Panics if this `JoinError` was not created as a result of the underlying
    /// task terminating with a panic. Use `is_panic` to check the error reason
    /// or `try_into_panic` for a variant that does not panic.
    pub fn into_panic(self) -> Payload {
        self.try_into_panic()
            .expect("JoinError reason is not a panic")
    }

    /// Consume this error, returning the object with which the task panicked if
    /// the task terminated due to a panic. Otherwise, `self` is returned.
    pub fn try_into_panic(self) -> Result<Payload, Self> {
        match self.0 {
            Detail::Panic(panic) => Ok(panic.into_inner()),
            _ => Err(self),
        }
    }
}

impl fmt::Display for JoinError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            Detail::Cancelled => write!(f, "task was cancelled"),
            Detail::Panic(_) => write!(f, "task panicked"),
        }
    }
}

impl fmt::Debug for JoinError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            Detail::Cancelled => f.write_str("Cancelled"),
            Detail::Panic(_) => f.debug_tuple("Panic").field(&format_args!("..")).finish(),
        }
    }
}
