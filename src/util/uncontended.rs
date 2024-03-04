use std::cell::UnsafeCell;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicBool, Ordering};

/// A mutex which panics if it is ever accessed concurrently.
///
/// This is meant for fields which are stored in the same structure but are
/// logically never accessed concurrently.
pub(crate) struct Uncontended<T> {
    locked: AtomicBool,
    value: UnsafeCell<T>,
}

impl<T> Uncontended<T> {
    pub fn new(value: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            value: UnsafeCell::new(value),
        }
    }

    /// Lock this `Uncontended`.
    ///
    /// # Panics
    /// Panics if `self` is already locked.
    pub fn lock(&self) -> UncontendedGuard<'_, T> {
        // Note: needs to be acquire to synchronize with dropping the guard
        match self.locked.swap(true, Ordering::Acquire) {
            true => panic!("Attempted to lock an Uncontended that was already locked"),
            false => UncontendedGuard(self),
        }
    }
}

// These mirror the contract on std::sync::Mutex
unsafe impl<T: Send> Send for Uncontended<T> {}
unsafe impl<T: Send> Sync for Uncontended<T> {}

pub struct UncontendedGuard<'a, T>(&'a Uncontended<T>);

impl<'a, T> Deref for UncontendedGuard<'a, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        // SAFETY: We have the lock so nobody else can access value
        unsafe { &*self.0.value.get() }
    }
}

impl<'a, T> DerefMut for UncontendedGuard<'a, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // SAFETY: We have the lock so nobody else can access value
        unsafe { &mut *self.0.value.get() }
    }
}

impl<'a, T> Drop for UncontendedGuard<'a, T> {
    fn drop(&mut self) {
        // This synchronizes with the swap in Uncontended::lock to ensure that changes
        // made to `value` are visible.
        self.0.locked.store(false, Ordering::Release);
    }
}
