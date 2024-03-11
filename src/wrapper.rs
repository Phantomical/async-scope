use std::future::Future;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};

use futures_channel::oneshot;
use futures_util::task::AtomicWaker;
use pin_project_lite::pin_project;

use crate::error::Payload;
use crate::{FutureObj, JoinHandle, ScopeHandle};

pin_project! {
    pub(crate) struct WrapFuture<'scope, 'env, F: Future> {
        #[pin]
        future: F,
        scope: ScopeHandle<'scope, 'env>,

        handle: Arc<TaskAbortHandle>,
        result: Option<oneshot::Sender<Result<F::Output, Payload>>>,
    }
}

impl<'scope, 'env, F: Future> WrapFuture<'scope, 'env, F> {
    fn new(
        future: F,
        scope: ScopeHandle<'scope, 'env>,
    ) -> (Self, JoinHandle<'scope, 'env, F::Output>) {
        let (tx, rx) = oneshot::channel();
        let abort = Arc::new(TaskAbortHandle {
            cancelled: AtomicBool::new(false),
            waker: AtomicWaker::new(),
        });

        (
            Self {
                future,
                scope,
                handle: abort.clone(),
                result: Some(tx),
            },
            JoinHandle::new(scope, abort, rx),
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
        scope: ScopeHandle<'scope, 'env>,
    ) -> (FutureObj<'scope, ()>, JoinHandle<'scope, 'env, F::Output>) {
        let (future, handle) = Self::new(future, scope);

        (Box::pin(future), handle)
    }
}

impl<'scope, 'env, F: Future> Future for WrapFuture<'scope, 'env, F> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        this.handle.waker.register(cx.waker());

        let cancelled = this.handle.cancelled.load(Ordering::Relaxed);
        if cancelled {
            *this.result = None;
            return Poll::Ready(());
        }

        let result = match catch_unwind(AssertUnwindSafe(|| this.future.poll(cx))) {
            Ok(Poll::Ready(value)) => Ok(value),
            Ok(Poll::Pending) => return Poll::Pending,
            Err(payload) => Err(payload),
        };

        let channel = match this.result.take() {
            Some(channel) => channel,
            None => panic!("Wrapper polled after returning Ready"),
        };

        if let Err(Err(payload)) = channel.send(result) {
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
