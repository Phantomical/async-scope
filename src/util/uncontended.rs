use std::sync::{Mutex, MutexGuard, TryLockError};

/// A mutex which panics if it is ever accessed concurrently.
///
/// This is meant for fields which are stored in the same structure but are
/// logically never accessed concurrently.
pub(crate) struct Uncontended<T>(Mutex<T>);

impl<T> Uncontended<T> {
    pub fn new(value: T) -> Self {
        Self(Mutex::new(value))
    }

    /// Lock this `Uncontended`.
    ///
    /// # Panics
    /// Panics if `self` is already locked.
    pub fn lock(&self) -> MutexGuard<'_, T> {
        match self.0.try_lock() {
            Ok(guard) => guard,
            Err(TryLockError::Poisoned(error)) => error.into_inner(),
            Err(TryLockError::WouldBlock) => {
                panic!("Attempted to lock an Uncontended that was already locked")
            }
        }
    }
}
