use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use futures_channel::oneshot;

use crate::error::Payload;
use crate::executor::{self, Executor};
use crate::wrapper::{TaskAbortHandle, WrapFuture};
use crate::JoinError;

/// A collection of tasks that are run together.
///
/// This type is returned by the [`scope`] macro. If you already have an
/// existing async function then you can use [`AsyncScope::new`] instead.
pub struct AsyncScope<'a, T> {
    executor: Executor<'a>,
    main: JoinHandle<'a, T>,
    cancel_remaining_tasks_on_exit: bool,
}

impl<'a, T> AsyncScope<'a, T> {
    pub fn new<F, Fut>(func: F) -> Self
    where
        F: FnOnce(ScopeHandle<'a>) -> Fut,
        Fut: Future<Output = T> + Send + 'a,
        T: Send + 'a,
    {
        let mut executor = Executor::new();
        let future = func(ScopeHandle::new(executor.handle()));
        let (future, handle) = WrapFuture::new(future);

        executor.spawn(Box::pin(future));

        Self {
            executor,
            main: handle,
            cancel_remaining_tasks_on_exit: false,
        }
    }

    pub fn spawn<F>(&mut self, future: F) -> JoinHandle<'a, F::Output>
    where
        F: Future + Send + 'a,
        F::Output: Send,
    {
        let (future, handle) = WrapFuture::new(future);
        self.executor.spawn(Box::pin(future));
        handle
    }
}

impl<'a, R> Future for AsyncScope<'a, R> {
    type Output = R;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.executor.poll(cx) {
            Poll::Pending if !self.cancel_remaining_tasks_on_exit => return Poll::Pending,
            _ => (),
        }

        match Pin::new(&mut self.main).poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Ok(value)) => Poll::Ready(value),
            Poll::Ready(Err(e)) => match e.try_into_panic() {
                Ok(payload) => std::panic::resume_unwind(payload),
                Err(_) => panic!("main async scope task was cancelled"),
            },
        }
    }
}

#[derive(Clone)]
pub struct ScopeHandle<'a> {
    handle: executor::Handle<'a>,
}

impl<'a> ScopeHandle<'a> {
    fn new(handle: executor::Handle<'a>) -> Self {
        Self { handle }
    }

    pub fn spawn<F>(&self, future: F) -> JoinHandle<'a, F::Output>
    where
        F: Future + Send + 'a,
        F::Output: Send,
    {
        let (future, handle) = WrapFuture::new(future);
        self.handle.spawn(Box::pin(future));
        handle
    }
}

pub struct JoinHandle<'a, T> {
    handle: Arc<TaskAbortHandle>,
    channel: oneshot::Receiver<Result<T, Payload>>,
    _marker: PhantomData<&'a ()>,
}

impl<'a, T> JoinHandle<'a, T> {
    pub(crate) fn new(
        handle: Arc<TaskAbortHandle>,
        channel: oneshot::Receiver<Result<T, Payload>>,
    ) -> Self {
        Self {
            handle,
            channel,
            _marker: PhantomData,
        }
    }

    pub fn abort(&self) {
        self.handle.abort();
    }

    pub fn abort_handle(&self) -> AbortHandle {
        AbortHandle(self.handle.clone())
    }
}

impl<'a, R> Future for JoinHandle<'a, R> {
    type Output = Result<R, JoinError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.channel)
            .poll(cx)
            .map(|result| match result {
                Ok(Ok(value)) => Ok(value),
                Ok(Err(payload)) => Err(JoinError::panicked(payload)),
                Err(_) => Err(JoinError::cancelled()),
            })
    }
}

impl<'a, R> Unpin for JoinHandle<'a, R> {}

#[derive(Clone)]
pub struct AbortHandle(Arc<TaskAbortHandle>);

impl AbortHandle {
    pub fn abort(&self) {
        self.0.abort()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(dead_code)]
    const fn require_send<T: Send>() {}

    const _: () = {
        require_send::<JoinHandle<()>>();
        require_send::<AbortHandle>();
    };
}
