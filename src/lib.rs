//! Async equivalent of [`std::thread::scope`].
//!
//! This crate allows you to write futures that use `spawn` but also borrow data
//! from the current function. It does this by running those futures in a local
//! executor within the current task.
//!
//! # Example
//! ```
//! # #[tokio::main]
//! # async fn main() {
//! use std::time::Duration;
//!
//! let mut a = vec![1, 2, 3];
//! let mut x = 0;
//!
//! let scope = async_scope::scope!(|scope| {
//!     scope.spawn(async {
//!         // We can borrow `a` here
//!         dbg!(&a);
//!     });
//!
//!     scope.spawn(async {
//!         // We can even mutably borrow `x` here because no other threads are
//!         // using it.
//!         x += a[0] + a[2];
//!     });
//!
//!     let handle = scope.spawn(async {
//!         // We can also run arbitrary futures as part of the scope tasks.
//!         tokio::time::sleep(Duration::from_millis(50)).await;
//!     });
//!
//!     // The main task can also await on futures
//!     tokio::time::sleep(Duration::from_millis(10)).await;
//!
//!     // and even wait for tasks that have been spawned
//!     handle
//!         .await
//!         .expect("the task panicked");
//! });
//!
//! // We do need to await the scope so that it can run the tasks, though.
//! scope.await;
//! # }
//! ```

#![deny(unsafe_code)]
// #![warn(missing_docs)]

use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use futures_channel::oneshot;

use crate::error::Payload;
use crate::executor::Executor;
use crate::wrapper::{TaskAbortHandle, WrapFuture};

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

/// Create a new scope for spawning scoped tasks.
///
/// The function will be passed a [`ScopeHandle`] which can be used to
/// [`spawn`]` new scoped tasks.
///
/// Unlike tokio's [`spawn`][tokio-spawn], scoped tasks can borrow non-`'static`
/// data, as the scope guarantees that all tasks will be either joined or
/// cancelled at the end of the scope.
///
/// All tasks that have not been manually joined will be automatically joined
/// before the [`AsyncScope`] completes.
///
/// # Example
/// ```
/// # #[tokio::main]
/// # async fn main() {
/// use std::time::Duration;
///
/// let mut a = vec![1, 2, 3];
/// let mut x = 0;
///
/// let scope = async_scope::scope!(|scope| {
///     scope.spawn(async {
///         // We can borrow `a` here
///         dbg!(&a);
///     });
///
///     scope.spawn(async {
///         // We can even mutably borrow `x` here because no other threads are
///         // using it.
///         x += a[0] + a[2];
///     });
///
///     let handle = scope.spawn(async {
///         // We can also run arbitrary futures as part of the scope tasks.
///         tokio::time::sleep(Duration::from_millis(50)).await;
///     });
///
///     // The main task can also await on futures
///     tokio::time::sleep(Duration::from_millis(10)).await;
///
///     // and even wait for tasks that have been spawned
///     handle
///         .await
///         .expect("the task panicked");
/// });
///
/// // We do need to await the scope so that it can run the tasks, though.
/// scope.await;
/// # }
/// ```
///
/// [`spawn`]: ScopeHandle::spawn
/// [tokio-spawn]: https://docs.rs/tokio/latest/tokio/task/fn.spawn.html
#[macro_export]
macro_rules! scope {
    (| $scope:ident | $body:expr) => {
        $crate::AsyncScope::new($crate::Options::new(), |$scope| async {
            let $scope = $scope;

            $body
        })
    };
}

/// A collection of tasks that are run together.
///
/// This type is returned by the [`scope`] macro. If you already have an
/// existing async function then you can use [`AsyncScope::new`] instead.
pub struct AsyncScope<'a, R> {
    executor: Executor<'a>,
    main: JoinHandle<'a, R>,
}

impl<'a, R> AsyncScope<'a, R> {
    pub fn new<F, Fut>(options: Options, future: F) -> Self
    where
        F: FnOnce(ScopeHandle<'a>) -> Fut,
        Fut: Future<Output = R> + Send + 'a,
        R: Send + 'a,
    {
        let (mut executor, handle) = Executor::new(options);

        let future = future(ScopeHandle { handle });
        let (future, handle) = WrapFuture::new(future);
        executor.spawn(Box::pin(future));

        Self {
            executor,
            main: handle,
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
        let poll = self.executor.poll(cx);
        let options = self.executor.options();

        if !options.cancel_remaining_tasks_on_exit {
            if poll.is_pending() {
                return Poll::Pending;
            }
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
    fn new(handle: Arc<TaskAbortHandle>, channel: oneshot::Receiver<Result<T, Payload>>) -> Self {
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
