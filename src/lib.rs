//! An async equivalent of [`std::thread::scope`].
//!
//! This crate allows you to write futures that use `spawn` but also borrow data
//! from the current function. It does this by running those futures in a local
//! executor within the current task.
//!
//! To do this, declare a new scope using the [`scope!`] macro
//! ```
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() {
//! let mut x = 0;
//!
//! async_scope::scope!(|scope| {
//!     scope.spawn(async { println!("task 1: x = {x}") });
//!     scope.spawn(async { println!("task 2: x = {x}") });
//! })
//! .await;
//! # }
//! ```
//!
//! The scope future will not resolve until all spawned tasks have either
//! completed or been cancelled.
//!
//! # Scope Cancellation
//! The scope created by [`scope!`] is just a regular future. Dropping it
//! before it has completed will drop all the tasks spawned within. Within a
//! scope, you can cancel individual spawned tasks by calling
//! [`JoinHandle::abort`].

/// Helper macro used to silence `unused_import` warnings when an item is
/// only imported in order to refer to it within a doc comment.
#[allow(unused_macros)]
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

pub use crate::error::JoinError;
pub use crate::scope::ScopeHandle;

#[cfg(feature = "stream")]
pub mod stream;

mod error;
mod executor;
mod scope;
mod util;
mod wrapper;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use crate::error::Payload;
use crate::scope::ScopeExecutor;
use crate::util::complete::RequirePending;
use crate::util::split_arc::{Full, Partial};
use crate::util::OneshotCell;
use crate::wrapper::{TaskAbortHandle, WrapFuture};

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
/// # #[tokio::main(flavor = "current_thread")]
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
    (| $scope:pat_param | $body:expr) => {
        $crate::exports::new_scope_unchecked(|__scope: $crate::ScopeHandle| {
            // Attempting to do `let $scope = __scope` actually ends up still borrowing
            // __scope instead of moving it.
            //
            // In order to actually ensure that we move __scope into the async scope we need
            // 2 things
            // 1. A non-copy type. Here that's SmuggleDataIntoBlock.
            // 2. A method that consumes it by value. We use the into_inner method.
            //
            // Missing either of these will still take a reference instead of moving the
            // value into the returned future.
            let __scope = $crate::exports::SmuggleDataIntoBlock(__scope);

            $crate::exports::pin_send(async {
                let $scope = __scope.into_inner();

                $body
            })
        })
    };
}

type FutureObj<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[cfg(test)]
mod tests;

#[doc(hidden)]
pub mod exports {
    use std::future::Future;

    use crate::{FutureObj, ScopeHandle};

    pub struct SmuggleDataIntoBlock<T>(pub T);

    impl<T> SmuggleDataIntoBlock<T> {
        pub fn into_inner(self) -> T {
            self.0
        }
    }

    pub fn pin_send<'a, F>(future: F) -> crate::FutureObj<'a, F::Output>
    where
        F: Future + Send + 'a,
    {
        Box::pin(future)
    }

    pub fn new_scope_unchecked<'env, F, T>(func: F) -> crate::AsyncScope<'env, T>
    where
        F: for<'scope> FnOnce(ScopeHandle<'scope, 'env>) -> FutureObj<'scope, T>,
        T: Send + 'env,
    {
        crate::AsyncScope::new(func)
    }
}

/// A collection of tasks that are run together.
///
/// This type is returned by the [`scope!`] macro.
pub struct AsyncScope<'env, T> {
    /// The `'env` lifetime here is a lie. `executor` is self-referential.
    executor: Arc<ScopeExecutor<'env>>,

    /// The second `'env` lifetime here is a lie. It really refers to
    /// `executor`.
    main: JoinHandle<'env, 'env, T>,
    cancel_remaining_tasks_on_exit: bool,
}

impl<'env, T> AsyncScope<'env, T> {
    /// Create a new `AsyncScope` from a function that takes a [`ScopeHandle`]
    /// and returns a future.
    ///
    /// Generally you will want to use [`scope!`] instead since it takes care of
    /// some tricky and borrowing issues as well as needing to box the initial
    /// future.
    pub fn new<F>(func: F) -> Self
    where
        F: for<'scope> FnOnce(ScopeHandle<'scope, 'env>) -> crate::FutureObj<'scope, T>,
        T: Send + 'env,
    {
        let executor = Arc::new(ScopeExecutor::new());

        // SAFETY: We need to ensure that the 'scope lifetime in this handle ends before
        //         the scope itself is dropped (not just at the same time). We do this
        //         by clearing the executor in our destructor before the Arc destructor
        //         runs.
        let scope = unsafe { executor.handle() };

        let future = func(scope);
        let (future, handle) = WrapFuture::new_send(future, scope);

        executor.spawn_direct(future);

        Self {
            executor,
            main: handle,
            cancel_remaining_tasks_on_exit: false,
        }
    }

    /// Configure whether remaining tasks should be polled to completion after
    /// the main scope task exits or just dropped.
    ///
    /// By default, whatever tasks are left in the [`AsyncScope`] continue to
    /// run until all tasks within have been polled to completion. This matches
    /// the existing behaviour of [`std::thread::scope`].
    ///
    /// Setting this to `true` means that once the initial scope task (i.e. the
    /// one passed in to [`scope!`]) completes then all other tasks will be
    /// dropped without them being polled to completion.
    pub fn cancel_remaining_tasks_on_exit(&mut self, enabled: bool) {
        self.cancel_remaining_tasks_on_exit = enabled;
    }
}

impl<'env, R> Future for AsyncScope<'env, R> {
    type Output = R;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.executor.poll(cx) {
            Poll::Pending if !self.cancel_remaining_tasks_on_exit => return Poll::Pending,
            _ => (),
        }

        match Pin::new(&mut self.main).poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Ok(value)) => match self.executor.unhandled_panic() {
                Some(payload) => std::panic::resume_unwind(payload),
                None => Poll::Ready(value),
            },
            Poll::Ready(Err(e)) => match e.try_into_panic() {
                Ok(payload) => std::panic::resume_unwind(payload),
                Err(_) => unreachable!("main async scope task was cancelled"),
            },
        }
    }
}

impl<'env, T> Drop for AsyncScope<'env, T> {
    fn drop(&mut self) {
        // The futures within the executor may have ScopeHandle references. We need to
        // drop them before the executor itself is dropped.
        self.executor.clear();
    }
}

/// An owned permission to join a scoped task (await its termination).
///
/// This can be thought of as an equivalent to [`std::thread::ScopedJoinHandle`]
/// for a scoped task. Note that the scoped task associated with this
/// `JoinHandle` started running immediately once you called `spawn`, even if
/// the [`JoinHandle`] has not been awaited yet.
pub struct JoinHandle<'scope, 'env, T> {
    scope: ScopeHandle<'scope, 'env>,
    handle: Full<TaskAbortHandle, OneshotCell<Result<T, Payload>>>,

    /// In debug mode we check to ensure that we aren't polled once the future
    /// is complete.
    check: RequirePending,
}

impl<'scope, 'env, T> JoinHandle<'scope, 'env, T> {
    pub(crate) fn new(
        scope: ScopeHandle<'scope, 'env>,
        handle: Full<TaskAbortHandle, OneshotCell<Result<T, Payload>>>,
    ) -> Self {
        Self {
            scope,
            handle,
            check: RequirePending::new(),
        }
    }

    /// Abort the task associated with the handle.
    ///
    /// Awaiting a cancelled task might complete as usual if the task was
    /// already completed at the time it was cancelled, but most likely it will
    /// fail with a [`cancelled`] [`JoinError`].
    ///
    /// If the task was already cancelled (e.g. by a previous call to `abort`)
    /// then this method will do nothing.
    ///
    /// [`cancelled`]: JoinError::is_cancelled
    pub fn abort(&self) {
        self.handle.abort();
    }

    /// Get an [`AbortHandle`] for this task.
    pub fn abort_handle(&self) -> AbortHandle {
        AbortHandle(Full::downgrade(&self.handle))
    }

    /// Whether the task has exited.
    pub fn is_finished(&self) -> bool {
        Full::value(&self.handle).is_complete()
    }
}

impl<'scope, 'env, T> Future for JoinHandle<'scope, 'env, T> {
    type Output = Result<T, JoinError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        debug_assert!(
            !self.check.is_ready(),
            "polled a JoinHandle that had already completed"
        );

        let result = match Full::value(&self.handle).poll_take(cx) {
            Poll::Ready(result) => result,
            Poll::Pending => return Poll::Pending,
        };

        self.check.set_ready();

        Poll::Ready(match result {
            Some(Ok(value)) => Ok(value),
            Some(Err(payload)) => Err(JoinError::panicked(payload)),
            None => Err(JoinError::cancelled()),
        })
    }
}

impl<'scope, 'env, T> Unpin for JoinHandle<'scope, 'env, T> {}

impl<'scope, 'env, T> Drop for JoinHandle<'scope, 'env, T> {
    fn drop(&mut self) {
        let cell = Full::value(&self.handle);
        cell.close();

        if let Ok(Err(payload)) = cell.take() {
            self.scope.mark_unhandled_panic(payload);
        }
    }
}

/// An owned permission to abort a spawned task, without awaiting its
/// copmletion.
///
/// Unlike a [`JoinHandle`], an `AbortHandle` does not allow you to await the
/// task's completion, only to abort it.
#[derive(Clone)]
pub struct AbortHandle(Partial<TaskAbortHandle>);

impl AbortHandle {
    /// Abort the task associated with the handle.
    ///
    /// Awaiting a cancelled task might complete as usual if the task was
    /// already completed at the time it was cancelled, but most likely it will
    /// fail with a [`cancelled`] [`JoinError`].
    ///
    /// If the task was already cancelled (e.g. by a previous call to `abort`)
    /// then this method will do nothing.
    ///
    /// [`cancelled`]: JoinError::is_cancelled
    pub fn abort(&self) {
        self.0.abort()
    }
}
