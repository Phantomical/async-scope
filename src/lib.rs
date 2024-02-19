#![warn(unsafe_op_in_unsafe_fn)]

use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use futures::FutureExt;

use crate::executor::Executor;
use crate::wrapper::{TaskCommon, TaskHandle, WrapFuture};

/// Helper macro used to silence `unused_import` warnings when an item is
/// only imported in order to refer to it within a doc comment.
macro_rules! used_in_docs {
    ($( $item:ident ),*) => {
        const _: () = {
            #[allow(unused_imports)]
            mod dummy {
                $( use super::$item; )*
            }
        };
    };
}

mod error;
mod executor;
mod options;
mod util;
mod wrapper;

pub use crate::error::JoinError;
pub use crate::options::Options;

pub fn scope<'a, F, Fut>(func: F) -> AsyncScope<'a, Fut::Output>
where
    F: FnOnce(ScopeHandle<'a>) -> Fut,
    Fut: Future + Send + 'a,
    Fut::Output: Send,
{
    AsyncScope::new(Options::default(), func)
}

pub struct AsyncScope<'a, R> {
    executor: Executor<'a>,
    main: JoinHandle<'a, R>,
}

impl<'a, R> AsyncScope<'a, R> {
    fn new<F, Fut>(options: Options, future: F) -> Self
    where
        F: FnOnce(ScopeHandle<'a>) -> Fut,
        Fut: Future<Output = R> + Send + 'a,
        R: Send + 'a,
    {
        let (mut executor, handle) = Executor::new(options);

        let future = future(ScopeHandle { handle });
        let (future, handle) = WrapFuture::new(future, executor.options());
        executor.spawn(Box::pin(future));

        Self {
            executor,
            main: JoinHandle::new(handle),
        }
    }

    pub fn spawn<F>(&mut self, future: F) -> JoinHandle<'a, F::Output>
    where
        F: Future + Send + 'a,
        F::Output: Send,
    {
        let (future, handle) = WrapFuture::new(future, self.executor.options());
        self.executor.spawn(Box::pin(future));

        JoinHandle {
            handle,
            _marker: PhantomData,
        }
    }
}

impl<'a, R> Future for AsyncScope<'a, R> {
    type Output = R;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let poll = self.executor.poll(cx);
        let options = self.executor.options();

        if !options.cancel_remaining_tasks_on_exit {
            if poll.is_pending() {
                return Poll::Pending;
            }
        }

        match self.main.poll_unpin(cx) {
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
    pub fn spawn<F>(&self, future: F) -> JoinHandle<'a, F::Output>
    where
        F: Future + Send + 'a,
        F::Output: Send,
    {
        let (future, handle) = WrapFuture::new(future, self.handle.options());
        self.handle.spawn(Box::pin(future));

        JoinHandle {
            handle,
            _marker: PhantomData,
        }
    }
}

pub struct JoinHandle<'a, T> {
    handle: Arc<TaskHandle<T>>,
    _marker: PhantomData<&'a AsyncScope<'a, T>>,
}

impl<'a, T> JoinHandle<'a, T> {
    fn new(handle: Arc<TaskHandle<T>>) -> Self {
        Self {
            handle,
            _marker: PhantomData,
        }
    }

    pub fn abort(&self) {
        self.handle.abort();
    }

    pub fn abort_handle(&self) -> AbortHandle {
        AbortHandle(self.handle.common())
    }
}

impl<'a, R> Future for JoinHandle<'a, R> {
    type Output = Result<R, JoinError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // SAFETY: JoinHandle::poll is the only place we can call handle.poll.
        //         This ensures that it is never called concurrently.
        unsafe { self.handle.poll(cx) }
    }
}

impl<'a, R> Unpin for JoinHandle<'a, R> {}

#[derive(Clone)]
pub struct AbortHandle(Arc<TaskCommon>);

impl AbortHandle {
    pub fn abort(&self) {
        self.0.abort()
    }
}
