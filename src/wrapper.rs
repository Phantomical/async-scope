use std::future::Future;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};

use atomic_waker::AtomicWaker;
use pin_project_lite::pin_project;

use crate::error::Payload;
use crate::executor::UnhandledPanicFlag;
use crate::util::split_arc::Full;
use crate::util::OneshotCell;
use crate::JoinHandle;

pin_project! {
    pub(crate) struct WrapFuture<F: Future> {
        #[pin]
        future: F,
        shared: Full<TaskAbortHandle, OneshotCell<Result<F::Output, Payload>>>,
    }
}

impl<F: Future> WrapFuture<F> {
    pub fn new<'a>(
        future: F,
        unhandled_panic: UnhandledPanicFlag,
    ) -> (Self, JoinHandle<'a, F::Output>) {
        let abort = Full::new(
            TaskAbortHandle {
                cancelled: AtomicBool::new(false),
                waker: AtomicWaker::new(),
                unhandled_panic,
            },
            OneshotCell::new(),
        );

        (
            Self {
                future,
                shared: abort.clone(),
            },
            JoinHandle::new(abort),
        )
    }
}

impl<F: Future> Future for WrapFuture<F> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        let shared = &*this.shared;

        shared.waker.register(cx.waker());

        let cancelled = this.shared.cancelled.load(Ordering::Relaxed);
        let cell = Full::value(shared);

        if cancelled {
            cell.close();
            return Poll::Ready(());
        }

        let result = match catch_unwind(AssertUnwindSafe(|| this.future.poll(cx))) {
            Ok(Poll::Ready(value)) => Ok(value),
            Ok(Poll::Pending) => return Poll::Pending,
            Err(payload) => Err(payload),
        };

        if let Err(Err(payload)) = cell.store(result) {
            shared.unhandled_panic.mark_unhandled_panic(payload);
        }

        Poll::Ready(())
    }
}

pub(crate) struct TaskAbortHandle {
    waker: AtomicWaker,
    cancelled: AtomicBool,
    unhandled_panic: UnhandledPanicFlag,
}

impl TaskAbortHandle {
    pub fn abort(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
        self.waker.wake();
    }

    pub fn mark_unhandled_panic(&self, payload: Payload) {
        self.unhandled_panic.mark_unhandled_panic(payload);
    }
}
