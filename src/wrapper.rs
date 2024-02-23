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

use std::future::Future;
use std::panic::{self, AssertUnwindSafe};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};

use atomic_waker::AtomicWaker;
use futures_channel::oneshot;
use pin_project_lite::pin_project;

use crate::error::Payload;
use crate::executor::UnhandledPanicFlag;
use crate::JoinHandle;

type PayloadResult<F> = Result<<F as Future>::Output, Payload>;

pin_project! {
    pub(crate) struct WrapFuture<F: Future> {
        #[pin]
        future: F,
        abort: Arc<TaskAbortHandle>,
        channel: Option<oneshot::Sender<PayloadResult<F>>>,
    }
}

impl<F: Future> WrapFuture<F> {
    pub fn new<'a>(
        future: F,
        unhandled_panic: UnhandledPanicFlag,
    ) -> (Self, JoinHandle<'a, F::Output>) {
        let abort = Arc::new(TaskAbortHandle {
            cancelled: AtomicBool::new(false),
            waker: AtomicWaker::new(),
            unhandled_panic,
        });

        let (tx, rx) = oneshot::channel();

        (
            Self {
                future,
                abort: abort.clone(),
                channel: Some(tx),
            },
            JoinHandle::new(abort, rx),
        )
    }
}

impl<F: Future> Future for WrapFuture<F> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        let cancelled = this.abort.cancelled.load(Ordering::Relaxed);

        if cancelled {
            this.channel.take();
            return Poll::Ready(());
        }

        let result = match panic::catch_unwind(AssertUnwindSafe(|| this.future.poll(cx))) {
            Ok(Poll::Ready(value)) => Ok(value),
            Ok(Poll::Pending) => {
                this.abort.waker.register(cx.waker());
                return Poll::Pending;
            }
            Err(payload) => Err(payload),
        };

        let tx = this
            .channel
            .take()
            .expect("future completed but channel was already used");

        if let Err(Err(payload)) = tx.send(result) {
            std::panic::resume_unwind(payload);
        }

        Poll::Ready(())
    }
}

pub(crate) struct TaskAbortHandle {
    cancelled: AtomicBool,
    waker: AtomicWaker,
    unhandled_panic: UnhandledPanicFlag,
}

impl TaskAbortHandle {
    pub fn abort(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
        self.waker.wake();
    }

    pub fn mark_unhandled_panic(&self) {
        self.unhandled_panic.mark_unhandled_panic();
    }
}
