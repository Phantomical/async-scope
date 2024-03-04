use std::future::Future;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};

use futures_util::task::{AtomicWaker, FutureObj};
use pin_project_lite::pin_project;

use crate::error::Payload;
use crate::util::split_arc::Full;
use crate::util::OneshotCell;
use crate::{JoinHandle, ScopeHandle};

pin_project! {
    pub(crate) struct WrapFuture<'scope, 'env, F: Future> {
        #[pin]
        future: F,
        scope: &'scope ScopeHandle<'env>,
        shared: Full<TaskAbortHandle, OneshotCell<Result<F::Output, Payload>>>,
    }
}

impl<'scope, 'env, F: Future> WrapFuture<'scope, 'env, F> {
    fn new(
        future: F,
        scope: &'scope ScopeHandle<'env>,
    ) -> (Self, JoinHandle<'scope, 'env, F::Output>) {
        let abort = Full::new(
            TaskAbortHandle {
                cancelled: AtomicBool::new(false),
                waker: AtomicWaker::new(),
            },
            OneshotCell::new(),
        );

        (
            Self {
                future,
                scope,
                shared: abort.clone(),
            },
            JoinHandle::new(scope, abort),
        )
    }
}

impl<'scope, 'env, F> WrapFuture<'scope, 'env, F>
where
    F: Future + Send + 'scope,
    F::Output: Send + 'scope,
{
    pub fn new_send(
        future: F,
        scope: &'scope ScopeHandle<'env>,
    ) -> (FutureObj<'scope, ()>, JoinHandle<'scope, 'env, F::Output>) {
        let (future, handle) = Self::new(future, scope);

        (FutureObj::from(Box::pin(future)), handle)
    }
}

impl<'scope, 'env, F: Future> Future for WrapFuture<'scope, 'env, F> {
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
            this.scope.mark_unhandled_panic(payload);
        }

        Poll::Ready(())
    }
}

pub(crate) struct TaskAbortHandle {
    waker: AtomicWaker,
    cancelled: AtomicBool,
}

impl TaskAbortHandle {
    pub fn abort(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
        self.waker.wake();
    }
}
