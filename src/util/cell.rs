use std::cell::UnsafeCell;
use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicU8, Ordering};
use std::task::{Context, Poll};

use atomic_waker::AtomicWaker;

/// Initial state. In this state:
/// - A store may start by transitioning into LOCKED
/// - The cell can be closed by transitioning directly to TAKEN
/// - Nobody may touch `value`.
const UNINIT: u8 = 0;

/// A `store` is occurring at the moment. In this state:
/// - The thread that did the state transition has mutable access to `value`.
/// - Nobody else may touch `value`.
///
/// This state ends when the writer thread transitions the state to READY.
const LOCKED: u8 = 1;

/// A value is stored in the cell. In this state:
/// - A load may start by transitioning into TAKEN.
/// - Nobody else may touch `value` (unless they have mutable access to the
///   whole `OneshotCell`.)
const READY: u8 = 2;

/// The value is gone. In this state:
/// - The thread that did the state transition from READY has mutable access to
///   `value`.
/// - Nobody else may touch `value`.
const TAKEN: u8 = 3;

/// A cell that allows sending exactly one value through it.
///
/// This is essentially an in-place version of a oneshot channel.
pub(crate) struct OneshotCell<T> {
    waker: AtomicWaker,
    value: UnsafeCell<MaybeUninit<T>>,
    state: AtomicU8,
}

impl<T> OneshotCell<T> {
    pub fn new() -> Self {
        Self {
            waker: AtomicWaker::new(),
            value: UnsafeCell::new(MaybeUninit::uninit()),
            state: AtomicU8::new(UNINIT),
        }
    }

    /// Store a value within this `OneshotCell`.
    ///
    /// If there has already been a value stored in the cell (even if it has
    /// been taken already) attempting to store another value within the cell
    /// will fail.
    ///
    /// # Memory Ordering
    /// - On success this is equivalent to a release store.
    /// - On failure this synchronizes with the first `store`.
    ///
    /// In either case this will perform a store or synchronize with the
    /// previous store.
    pub fn store(&self, value: T) -> Result<(), T> {
        // Attempt to lock the cell
        if self
            .state
            .compare_exchange(UNINIT, LOCKED, Ordering::Relaxed, Ordering::Acquire)
            .is_err()
        {
            return Err(value);
        }

        // SAFETY: We transitioned from UNINIT so we have exclusive access to
        // self.value.
        let slot = unsafe { &mut *self.value.get() };

        // Since we just transitioned from UNINIT we know there's nothing stored in slot
        // and so this doesn't leak anything.
        slot.write(value);

        // This synchonizes with
        // - the load transition in `try_load`
        // - a failed load in `store`.
        self.state.store(READY, Ordering::Release);
        self.waker.wake();
        Ok(())
    }

    /// Attempt to take the value stored within this `OneshotCell`.
    ///
    /// If you are using the cell within an async context prefer using
    /// [`poll_take`][OneshotCell::poll_take] instead.
    ///
    /// # Errors
    /// This returns an error if there is no value to take from the cell. The
    /// error itself allows you to tell if this was due to the cell not having a
    /// value yet or the cell having lost its value.
    ///
    /// # Memory Ordering
    /// - On success this synchronizes with the corresponding `store`.
    /// - On failure this synchronizes with the original store only if it
    ///   returns [`TakeError::Closed`].
    pub fn take(&self) -> Result<T, TakeError> {
        // Attempt to lock the cell into the TAKEN state.
        //
        // This synchronizes with the store of READY in `store`.
        match self
            .state
            .compare_exchange(READY, TAKEN, Ordering::Acquire, Ordering::Acquire)
        {
            Ok(_) => (),
            Err(UNINIT | LOCKED) => return Err(TakeError::Pending),
            Err(TAKEN) => return Err(TakeError::Closed),
            Err(_) => unreachable!("OneshotCell in an invalid state"),
        }

        // SAFETY: We're in the TAKEN state and we made the transition so that
        //         means that we have exclusive access to self.value.
        let slot = unsafe { &mut *self.value.get() };

        // SAFETY: Since we transitioned from READY slot is guaranteed to be
        //         initialized.
        let value = unsafe { slot.assume_init_read() };

        Ok(value)
    }

    /// Close the cell without sending a value.
    ///
    /// This only has an effect if the cell is empty. Otherwise it does nothing.
    pub fn close(&self) -> bool {
        match self
            .state
            .compare_exchange(UNINIT, TAKEN, Ordering::Release, Ordering::Relaxed)
        {
            Ok(_) => {
                self.waker.wake();
                true
            }
            Err(_) => false,
        }
    }

    pub fn is_complete(&self) -> bool {
        matches!(self.state.load(Ordering::Acquire), TAKEN | READY)
    }

    pub fn poll_take(&self, cx: &mut Context<'_>) -> Poll<Option<T>> {
        self.waker.register(cx.waker());

        match self.take() {
            Ok(value) => Poll::Ready(Some(value)),
            Err(TakeError::Closed) => Poll::Ready(None),
            Err(TakeError::Pending) => Poll::Pending,
        }
    }
}

// SAFETY: OneshotCell contains a T so it should be send if T is Send
unsafe impl<T: Send> Send for OneshotCell<T> {}
// SAFETY: OneshotCell provides no way to access the T within other than by
//         moving it out. This means it is safe to have &OneshotCell<T> be Send
//         so OneshotCell can be Sync even if T is only Send.
unsafe impl<T: Send> Sync for OneshotCell<T> {}

impl<T> Drop for OneshotCell<T> {
    fn drop(&mut self) {
        if *self.state.get_mut() == READY {
            // SAFETY: We are in the READY state so value is guaranteed to be initialized.
            unsafe { self.value.get_mut().assume_init_drop() };
        }
    }
}

pub(crate) enum TakeError {
    Pending,
    Closed,
}
