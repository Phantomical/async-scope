use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use atomic_waker::AtomicWaker;
use concurrent_queue::ConcurrentQueue;
use futures_util::stream::futures_unordered::FuturesUnordered;
use futures_util::StreamExt;

use crate::error::Payload;

type FutureObj<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

struct Shared {
    waker: AtomicWaker,
    unhandled_panic: Mutex<Option<Payload>>,
}

pub(crate) struct Executor<'a> {
    exec: FuturesUnordered<FutureObj<'a>>,
    shared: Arc<Shared>,
    spawn: Arc<ConcurrentQueue<FutureObj<'a>>>,
}

impl<'a> Executor<'a> {
    pub fn new() -> Self {
        Self {
            shared: Arc::new(Shared {
                waker: AtomicWaker::new(),
                unhandled_panic: Mutex::new(None),
            }),
            spawn: Arc::new(ConcurrentQueue::unbounded()),
            exec: FuturesUnordered::new(),
        }
    }

    pub fn handle(&self) -> Handle<'a> {
        Handle {
            spawn: self.spawn.clone(),
            shared: self.shared.clone(),
        }
    }

    pub fn unhandled_panic(&self) -> Option<Payload> {
        let mut panic = self
            .shared
            .unhandled_panic
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        panic.take()
    }

    pub fn unhandled_panic_flag(&self) -> UnhandledPanicFlag {
        UnhandledPanicFlag(self.shared.clone())
    }

    pub fn spawn(&mut self, future: FutureObj<'a>) {
        self.exec.push(future);
    }

    fn spawn_all(&mut self) -> usize {
        let mut count = 0;

        while let Ok(future) = self.spawn.pop() {
            self.exec.push(future);
            count += 1;
        }

        count
    }

    pub fn poll(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        self.shared.waker.register(cx.waker());

        let poll = self.exec.poll_next_unpin(cx);
        let more = self.spawn_all() != 0;

        if more {
            // We have some freshly spawned tasks so we should wake up immediately.
            cx.waker().wake_by_ref();
        }

        match poll {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Some(())) => {
                // We don't want to poll exec multiple times without yielding to the executor so
                // if a task completes we yield to the executor but notify it that we want to
                // wake up immediately.
                cx.waker().wake_by_ref();
                Poll::Pending
            }
            // The executor is empty but more tasks have just been spawned so there is still work to
            // do. The waker has already been awoken up above.
            Poll::Ready(None) if more => Poll::Pending,
            // No tasks and nothing spawned, we're done.
            Poll::Ready(None) => Poll::Ready(()),
        }
    }
}

impl<'a> Drop for Executor<'a> {
    fn drop(&mut self) {
        // Prevent spawns and wakeups from accumulating
        self.spawn.close();
    }
}

#[derive(Clone)]
pub(crate) struct Handle<'a> {
    spawn: Arc<ConcurrentQueue<FutureObj<'a>>>,
    shared: Arc<Shared>,
}

impl<'a> Handle<'a> {
    /// Spawn a new future onto the [`Executor`] for this handle.
    ///
    /// # Panics
    /// Panics if the executor has already been dropped.
    pub fn spawn(&self, future: FutureObj<'a>) {
        if self.spawn.push(future).is_err() {
            panic!("attempted to spawn a future on a dead AsyncScope")
        }

        self.shared.waker.wake();
    }

    pub fn unhandled_panic_flag(&self) -> UnhandledPanicFlag {
        UnhandledPanicFlag(self.shared.clone())
    }
}

pub(crate) struct UnhandledPanicFlag(Arc<Shared>);

impl UnhandledPanicFlag {
    pub fn mark_unhandled_panic(&self, payload: Payload) {
        let mut panic = self
            .0
            .unhandled_panic
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        if panic.is_none() {
            *panic = Some(payload);
        }
    }
}
