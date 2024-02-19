//! Top-level future wrapper.
//!
//! This allows us to implement cancellation and panic handling without having
//! to deal with it at the executor level.
//!
//! This module contains two main types
//! - [`WrapFuture`] is wrapper around a future that handles panics, deals with
//!   cancellation, and forwards the return value.
//! - [`TaskHandle`] is basically the internal cell of a oneshot channel with
//!   some extra spots for sideband information such a cancellation requests.

use std::cell::UnsafeCell;
use std::future::Future;
use std::mem::MaybeUninit;
use std::ops::Deref;
use std::panic::{self, AssertUnwindSafe};
use std::pin::Pin;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};

use atomic_waker::AtomicWaker;

use crate::error::Payload;
use crate::{JoinError, Options};

// Individual states for the cell.
//
// The values are chosen so that fetch_or(state) will always go to the next
// state. This allows us to avoid compare_exchange loops on some hardware.
const PENDING: u8 = 0;
const READY: u8 = 1;
const TAKEN: u8 = 3;

const STATE_MASK: u8 = 0x3;

/// Indicates that the consumer has requested that the task cancel itself.
const CANCEL_REQUEST_FLAG: u8 = 1 << 2;

/// Indicates that the task was cancelled.
const CANCELLED_FLAG: u8 = 1 << 3;

/// Indicates that the task should rethrow caught panics.
const RETHROW_FLAG: u8 = 1 << 4;

pub(crate) struct WrapFuture<F: Future> {
    future: F,
    handle: Arc<TaskHandle<F::Output>>,
}

impl<F: Future> WrapFuture<F> {
    pub fn new(future: F, options: &Options) -> (Self, Arc<TaskHandle<F::Output>>) {
        let mut state = PENDING;
        if options.catch_unwind {
            state |= RETHROW_FLAG;
        }

        let handle = Arc::new(TaskHandle {
            common: Arc::new(TaskCommon {
                tx_waker: AtomicWaker::new(),
                rx_waker: AtomicWaker::new(),
                state: AtomicU8::new(state),
            }),
            value: UnsafeCell::new(MaybeUninit::uninit()),
        });

        (
            Self {
                future,
                handle: handle.clone(),
            },
            handle,
        )
    }

    fn project(self: Pin<&mut Self>) -> (Pin<&mut F>, &TaskHandle<F::Output>) {
        // SAFETY: This is pretty standard pin projection.
        let this = unsafe { Pin::get_unchecked_mut(self) };

        (
            // SAFETY: We already have a Pin<&mut Self> so projecting it to a Pin<&mut F> is fine.
            unsafe { Pin::new_unchecked(&mut this.future) },
            &this.handle,
        )
    }
}

impl<F: Future> Future for WrapFuture<F> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let (future, task) = self.project();
        let state = task.state.load(Ordering::Acquire);

        if state & CANCEL_REQUEST_FLAG != 0 {
            task.state.fetch_or(CANCELLED_FLAG, Ordering::Relaxed);
            return Poll::Ready(());
        }

        let result = match panic::catch_unwind(AssertUnwindSafe(|| future.poll(cx))) {
            Ok(Poll::Ready(value)) => Ok(value),
            Ok(Poll::Pending) => {
                task.tx_waker.register(cx.waker());
                return Poll::Pending;
            }
            Err(payload) if state & RETHROW_FLAG != 0 => std::panic::resume_unwind(payload),
            Err(payload) => Err(payload),
        };

        // SAFETY: task.store_with is _only_ called from WrapFuture::poll. Since we have
        //         a Pin<&mut Self> we know that store_with will not be called
        //         concurrently.
        unsafe { task.store_with(state, result) };

        Poll::Ready(())
    }
}

impl<F: Future> Drop for WrapFuture<F> {
    fn drop(&mut self) {
        self.handle.set_cancelled()
    }
}

pub(crate) struct TaskCommon {
    /// Waker for the task that will be sending the value.
    tx_waker: AtomicWaker,

    /// Waker for the task that will be receiving the value.
    rx_waker: AtomicWaker,

    /// Current state of this task handle.
    state: AtomicU8,
}

impl TaskCommon {
    fn set_cancelled(&self) {
        self.state.fetch_or(CANCELLED_FLAG, Ordering::Relaxed);
    }

    pub(crate) fn abort(&self) {
        self.state.fetch_or(CANCEL_REQUEST_FLAG, Ordering::Relaxed);
        self.tx_waker.wake();
    }
}

pub(crate) struct TaskHandle<T> {
    common: Arc<TaskCommon>,

    /// Whether it is safe to access this depends on the current value of
    /// `state`.
    value: UnsafeCell<MaybeUninit<Result<T, Payload>>>,
}

impl<T> TaskHandle<T> {
    pub(crate) fn common(&self) -> Arc<TaskCommon> {
        self.common.clone()
    }

    /// Store a value into this `TaskHandle`.
    ///
    /// `state` should be loaded from `self.state` recently.
    ///
    /// # Safety
    /// - It is UB to call store concurrently.
    /// - State must have been loaded from `self` and there must not have been
    ///   any calls to `store` since then.
    unsafe fn store_with(&self, state: u8, value: Result<T, Payload>) {
        match state & STATE_MASK {
            PENDING => (),
            READY | TAKEN => unreachable!("repeat store to a TaskHandle"),
            _ => unreachable!("TaskHandle state was invalid"),
        }

        // SAFETY: Since we are in the PENDING state we know that nobody else
        //         will be touching self.value so it is safe to access.
        let slot = unsafe { &mut *self.value.get() };
        slot.write(value);

        // We've written a value into the slot, indicate this to whomever is waiting on
        // it. Note that this needs to be a fetch_or because CANCEL_FLAG could be set
        // concurrently.
        self.state.fetch_or(READY, Ordering::Release);
        self.rx_waker.wake();
    }

    /// Poll this `TaskHandle` to retrieve the stored payload.
    ///
    /// # Safety
    /// - It is not safe to call poll concurrently from multiple threads.
    pub(crate) unsafe fn poll(&self, cx: &mut Context<'_>) -> Poll<Result<T, JoinError>> {
        self.rx_waker.register(cx.waker());

        let state = self.state.load(Ordering::Acquire);
        match state & STATE_MASK {
            READY => (),
            // The task being cancelled only results in an error if we don't already have a result.
            PENDING if state & CANCELLED_FLAG != 0 => {
                return Poll::Ready(Err(JoinError::cancelled()))
            }
            PENDING => return Poll::Pending,
            TAKEN => panic!("TaskHandle polled after returning Poll::Ready"),
            _ => unreachable!("TaskHandle state was invalid"),
        }

        // SAFETY: Since we are in the READY state we know that no other threads will be
        //         modifying or accessing self.value.
        let slot = unsafe { &mut *self.value.get() };

        // SAFETY: The only way to transition into the READY state is via store
        //         which writes to self.value. So self.value must be
        //         initialized and this is safe.
        let value = unsafe { slot.assume_init_read() };

        // We have now read the value from slot so transition to the TAKEN state.
        self.state.fetch_or(TAKEN, Ordering::Release);

        Poll::Ready(value.map_err(JoinError::panicked))
    }
}

// SAFETY: We don't provide concurrent access to T so TaskHandle<T> can be both
//         Send and Sync as long as T is Send.
unsafe impl<T: Send> Send for TaskHandle<T> {}
unsafe impl<T: Send> Sync for TaskHandle<T> {}

impl<T> Deref for TaskHandle<T> {
    type Target = TaskCommon;

    fn deref(&self) -> &Self::Target {
        &self.common
    }
}

impl<T> Drop for TaskHandle<T> {
    fn drop(&mut self) {
        if self.state.load(Ordering::Relaxed) & STATE_MASK == READY {
            // SAFETY: Since the state is READY then value is initialized.
            unsafe { self.value.get_mut().assume_init_drop() };
        }
    }
}
