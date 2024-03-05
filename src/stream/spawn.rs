use std::collections::VecDeque;
use std::future::{poll_fn, Future};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use futures_core::Stream;
use futures_util::task::noop_waker;
use futures_util::StreamExt;

use crate::{JoinHandle, ScopeHandle};

/// Stream for [`scope_spawn`].
///
/// [`scope_spawn`]: super::ScopedStreamExt::scope_spawn
pub struct Spawn<'scope, 'env, S>
where
    S: Stream,
{
    channel: Arc<Channel<S::Item>>,
    handle: JoinHandle<'scope, 'env, ()>,
}

impl<'scope, 'env, S> Spawn<'scope, 'env, S>
where
    S: Stream + Send + 'scope,
    S::Item: Send + 'scope,
{
    pub(crate) fn new(stream: S, limit: usize, scope: ScopeHandle<'scope, 'env>) -> Self {
        let channel = Arc::new(Channel::new(limit));
        let handle = scope.spawn({
            let channel = channel.clone();
            async move { stream.for_each(|value| channel.push(value)).await }
        });

        Self { channel, handle }
    }
}

impl<'scope, 'env, S> Stream for Spawn<'scope, 'env, S>
where
    S: Stream + Send + 'scope,
    S::Item: Send + 'scope,
{
    type Item = S::Item;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if let Poll::Ready(value) = self.channel.poll_pop(cx) {
            return Poll::Ready(Some(value));
        }

        match Pin::new(&mut self.handle).poll(cx) {
            Poll::Ready(Ok(())) => Poll::Ready(None),
            Poll::Ready(Err(err)) => match err.try_into_panic() {
                Ok(payload) => std::panic::resume_unwind(payload),
                Err(_) => panic!("spawned future task was cancelled"),
            },
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<'scope, 'env, S> Drop for Spawn<'scope, 'env, S>
where
    S: Stream,
{
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/// An absolutely minimal channel implementation.
///
/// It is designed to be used under cases where there is no contention
/// whatsoever, such as communicating between different parts of the same future
/// (or multiple tasks in the same `AsyncScope`).
///
/// Note that it is technically possible to have tasks within an `AsyncScope`
/// run in different threads (either via unsafe or via `std::thread::scope`) so
/// this does actually need to be thread-safe.
///
/// As such, it is basically just a wrapper around a `VecDeque` behind a mutex.
struct Channel<T> {
    data: Mutex<ChannelData<T>>,
    limit: usize,
}

struct ChannelData<T> {
    tx_waker: Waker,
    rx_waker: Waker,
    queue: VecDeque<T>,
}

impl<T> ChannelData<T> {
    fn tx_waker(&mut self) -> Waker {
        std::mem::replace(&mut self.tx_waker, noop_waker())
    }

    fn rx_waker(&mut self) -> Waker {
        std::mem::replace(&mut self.rx_waker, noop_waker())
    }

    fn set_tx_waker(&mut self, waker: &Waker) {
        if !self.tx_waker.will_wake(waker) {
            self.tx_waker = waker.clone();
        }
    }

    fn set_rx_waker(&mut self, waker: &Waker) {
        if !self.rx_waker.will_wake(waker) {
            self.rx_waker = waker.clone();
        }
    }
}

impl<T> Channel<T> {
    pub fn new(limit: usize) -> Self {
        Self {
            data: Mutex::new(ChannelData {
                tx_waker: noop_waker(),
                rx_waker: noop_waker(),
                // Only preallocate capacity up to 32 elements to avoid large memory use if someone
                // passes in a large limit.
                queue: VecDeque::with_capacity(limit.saturating_add(1).min(32)),
            }),
            limit,
        }
    }

    pub async fn push(&self, value: T) {
        // push always immediately inserts the value and then we block until the queue
        // length has dropped below the configured limit.
        //
        // This ensures that setting a limit of 0 works as expected while still making
        // sure the producer blocks as expected.

        {
            let mut data = self.data.lock().unwrap_or_else(|e| e.into_inner());
            data.queue.push_back(value);
            data.rx_waker().wake();
        }

        poll_fn(|cx| {
            let mut data = self.data.lock().unwrap_or_else(|e| e.into_inner());

            if data.queue.len() <= self.limit {
                data.set_tx_waker(cx.waker());
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        })
        .await;
    }

    pub fn poll_pop(&self, cx: &mut Context<'_>) -> Poll<T> {
        let mut data = self.data.lock().unwrap_or_else(|e| e.into_inner());

        if let Some(value) = data.queue.pop_front() {
            data.tx_waker().wake();
            Poll::Ready(value)
        } else {
            data.set_rx_waker(cx.waker());
            Poll::Pending
        }
    }
}
