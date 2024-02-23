use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use futures_channel::oneshot;

use crate::error::Payload;
use crate::executor::{self, Executor};
use crate::wrapper::{TaskAbortHandle, WrapFuture};
use crate::{scope, JoinError};

used_in_docs!(scope);

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
    /// Create a new `AsyncScope` from a function that takes a [`ScopeHandle`]
    /// and returns a future.
    ///
    /// Usually you want to use [`scope!`] instead. It handles some footguns
    /// around borrowing.
    pub fn new<F, Fut>(func: F) -> Self
    where
        F: FnOnce(ScopeHandle<'a>) -> Fut,
        Fut: Future<Output = T> + Send + 'a,
        T: Send,
    {
        let mut executor = Executor::new();

        let future = func(ScopeHandle::new(executor.handle()));
        let (future, handle) = WrapFuture::new(future, executor.unhandled_panic_flag());

        executor.spawn(Box::pin(future));

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

    /// Set the maximum number of futures that can be polled before yielding to
    /// the parent executor.
    ///
    /// If many tasks are scheduled at once then it would be possible for
    /// [`AsyncScope::poll`] to block for a significant amount of time, which
    /// would cause problems for other tasks running on the executor. To avoid
    /// this, we yield unconditionally to the executor after having polled a
    /// number of tasks. This allows for other tasks to make progress and the
    /// executor will return to this one once it has worked through its queue.
    ///
    /// The default value is 32.
    ///
    /// # Panics
    /// Panics if `value` is 0.
    pub fn max_polls_without_yield(&mut self, value: u32) {
        assert_ne!(value, 0);
        self.executor.max_polls_without_yield(value);
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
            Poll::Ready(Ok(_)) if self.executor.unhandled_panic() => {
                panic!("a scoped task panicked")
            }
            Poll::Ready(Ok(value)) => Poll::Ready(value),
            Poll::Ready(Err(e)) => match e.try_into_panic() {
                Ok(payload) => std::panic::resume_unwind(payload),
                Err(_) => unreachable!("main async scope task was cancelled"),
            },
        }
    }
}

/// A handle to an [`AsyncScope`] that allows spawning scoped tasks on it.
///
/// This is provided to the closure passed to the [`scope!`] macro.
///
/// See the [crate] docs for details.
#[derive(Clone)]
pub struct ScopeHandle<'a> {
    handle: executor::Handle<'a>,
}

impl<'a> ScopeHandle<'a> {
    fn new(handle: executor::Handle<'a>) -> Self {
        Self { handle }
    }

    /// Spawn a new task within the scope, returning a [`JoinHandle`] to it.
    ///
    /// Unlike a non-scoped spawn, threads spawned with this function may borrow
    /// non-`'static` data from outside the scope. See the [`crate`] docs for
    /// details.
    ///
    /// The join handle can be awaited on to join the spawned task. If the
    /// spawned tasks panics then the output of awaiting the [`JoinHandle`] will
    /// be an [`Err`] containing the panic payload.
    ///
    /// If the join handle is dropped then the spawned task will be implicitly
    /// joined at the end of the scope. In that case, the scope will panic after
    /// all tasks have been joined. If
    /// [`AsyncScope::cancel_remaining_tasks_on_exit`] has been set to `true`,
    /// then the scope will not join tasks but will still panic after all tasks
    /// are canceled.
    ///
    /// If the [`JoinHandle`] outlives the scope and is then dropped, then the
    /// panic will be lost.
    pub fn spawn<F>(&self, future: F) -> JoinHandle<'a, F::Output>
    where
        F: Future + Send + 'a,
        F::Output: Send,
    {
        let (future, handle) = WrapFuture::new(future, self.handle.unhandled_panic_flag());
        self.handle.spawn(Box::pin(future));
        handle
    }
}

/// An owned permission to join a scoped task (await its termination).
///
/// This can be thought of as an equivalent to [`std::thread::ScopedJoinHandle`]
/// for a scoped task. Note that the scoped task associated with this
/// `JoinHandle` started running immediately once you called `spawn`, even if
/// the [`JoinHandle`] has not been awaited yet.
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
        AbortHandle(self.handle.clone())
    }
}

impl<'a, T> Future for JoinHandle<'a, T> {
    type Output = Result<T, JoinError>;

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

impl<'a, T> Unpin for JoinHandle<'a, T> {}

impl<'a, T> Drop for JoinHandle<'a, T> {
    fn drop(&mut self) {
        if let Ok(Some(Err(_payload))) = self.channel.try_recv() {
            self.handle.mark_unhandled_panic();
        }
    }
}

/// An owned permission to abort a spawned task, without awaiting its
/// copmletion.
///
/// Unlike a [`JoinHandle`], an `AbortHandle` does not allow you to await the
/// task's completion, only to abort it.
#[derive(Clone)]
pub struct AbortHandle(Arc<TaskAbortHandle>);

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
