use std::collections::VecDeque;
use std::future::Future;
use std::sync::Mutex;
use std::task::{Context, Poll};

use futures_util::stream::futures_unordered::FuturesUnordered;
use futures_util::task::AtomicWaker;
use futures_util::StreamExt;

use crate::util::Uncontended;

/// The core executor used to drive the scope.
///
/// This is mainly a wrapper around a [`FuturesUnordered`] with the addition of
/// a queue to hold onto newly spawned tasks before they get added onto the
/// executor.
pub(crate) struct Executor<F> {
    exec: Uncontended<FuturesUnordered<F>>,

    /// New tasks can only be spawned through `ScopeHandle` and it is not
    /// possible to send a `ScopeHandle` outside of the executor.
    ///
    /// This means that, usually, accesses to the queue should be uncontended
    /// and therefore a mutex is fast enough. However it is still possible that
    /// the handle gets passed to another thread (e.g. via `std::thread::scope`
    /// or unsafe) so we can't use `Uncontended` here.
    queue: Mutex<VecDeque<F>>,

    waker: AtomicWaker,
}

impl<F> Executor<F> {
    pub fn new() -> Self {
        Self {
            exec: Uncontended::new(FuturesUnordered::new()),
            waker: AtomicWaker::new(),
            queue: Mutex::new(VecDeque::new()),
        }
    }
}

impl<F> Executor<F>
where
    F: Future<Output = ()>,
{
    /// Spawn a new task onto the executor.
    ///
    /// This puts it into a queue which will be drained later, once polling is
    /// done.
    pub fn spawn(&self, future: F) {
        self.queue
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push_back(future);
        self.waker.wake();
    }

    /// Spawn a new task directly onto the executor.
    ///
    /// This is an **uncontended** method.
    ///
    /// # Panics
    /// Panics if called concurrently with any other **uncontended** methods.
    pub fn spawn_direct(&self, future: F) {
        self.exec.lock().push(future);
    }

    /// Clear all tasks from the executor.
    ///
    /// This is an **uncontended** method.
    ///
    /// # Panics
    /// Panics if called concurrently with any other **uncontended** methods.
    pub fn clear(&self) {
        self.exec.lock().clear();
    }

    /// Poll the tasks on the executor.
    ///
    /// This is an **uncontended** method.
    ///
    /// # Panics
    /// Panics if called concurrently with any other **uncontended** methods.
    pub fn poll(&self, cx: &mut Context<'_>) -> Poll<()> {
        self.waker.register(cx.waker());

        let mut exec = self.exec.lock();
        let poll = exec.poll_next_unpin(&mut *cx);

        // We need to be careful not to hold the queue lock while polling exec since
        // then a task attempting to spawn another would cause a deadlock.
        let mut queue = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        let more = !queue.is_empty();

        exec.extend(queue.drain(..));

        match poll {
            // More work just got added so we need to wake back up immediately.
            //
            // We don't poll exec in a loop here because that would result in tasks being polled
            // multiple times per top-level poll which can easily lead to exponential blowup once
            // you nest multiple layers of `AsyncScope`s.
            _ if more => {
                cx.waker().wake_by_ref();
                Poll::Pending
            }
            // The executor still has more to do, same logic as above.
            Poll::Ready(Some(())) => {
                cx.waker().wake_by_ref();
                Poll::Pending
            }
            Poll::Pending => Poll::Pending,
            // No tasks and nothing spawned, we're done.
            Poll::Ready(None) => Poll::Ready(()),
        }
    }
}
