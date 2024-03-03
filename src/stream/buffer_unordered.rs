use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures_core::{FusedStream, Stream};
use futures_util::stream::FuturesUnordered;
use pin_project_lite::pin_project;

use crate::{JoinHandle, ScopeHandle};

pin_project! {
    /// Stream for [`scope_buffer_unordered`][0].
    ///
    /// [0]: super::ScopedStreamExt::scope_buffer_unordered
    pub struct BufferUnordered<'scope, S>
    where
        S: Stream,
        S::Item: Future,
    {
        #[pin]
        stream: S,
        limit: usize,
        queue: FuturesUnordered<JoinHandle<'scope, <S::Item as Future>::Output>>,

        // scope = None means that the stream has been polled to completion
        scope: Option<ScopeHandle<'scope>>,
    }

    impl<'scope, S> PinnedDrop for BufferUnordered<'scope, S>
    where
        S: Stream,
        S::Item: Future
    {
        fn drop(this: Pin<&mut Self>) {
            let this = this.project();

            for handle in this.queue.iter() {
                handle.abort();
            }

            this.queue.clear();
        }
    }
}

impl<'scope, S> BufferUnordered<'scope, S>
where
    S: Stream,
    S::Item: Future + Send + 'scope,
    <S::Item as Future>::Output: Send + 'scope,
{
    pub(crate) fn new(stream: S, n: usize, scope: &ScopeHandle<'scope>) -> Self {
        BufferUnordered {
            stream,
            limit: n,
            queue: FuturesUnordered::new(),
            scope: Some(scope.clone()),
        }
    }
}

impl<'scope, S> Stream for BufferUnordered<'scope, S>
where
    S: Stream,
    S::Item: Future + Send + 'scope,
    <S::Item as Future>::Output: Send + 'scope,
{
    type Item = <S::Item as Future>::Output;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();

        if let Some(scope) = this.scope.as_ref() {
            while this.queue.len() < *this.limit {
                match this.stream.as_mut().poll_next(cx) {
                    Poll::Ready(Some(future)) => this.queue.push(scope.spawn(future)),
                    Poll::Ready(None) => {
                        *this.scope = None;
                        break;
                    }
                    Poll::Pending => break,
                }
            }
        }

        match Pin::new(this.queue).poll_next(cx) {
            Poll::Ready(Some(Ok(value))) => Poll::Ready(Some(value)),
            Poll::Ready(Some(Err(error))) => match error.try_into_panic() {
                Ok(payload) => std::panic::resume_unwind(payload),
                Err(_) => panic!("a task within the buffer was cancelled"),
            },
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let (lo, hi) = self.stream.size_hint();
        let count = self.queue.len();

        let lo = lo.saturating_add(count);
        let hi = hi.and_then(|hi| hi.checked_add(count));

        (lo, hi)
    }
}

impl<'scope, S> FusedStream for BufferUnordered<'scope, S>
where
    S: Stream,
    S::Item: Future + Send + 'scope,
    <S::Item as Future>::Output: Send + 'scope,
{
    fn is_terminated(&self) -> bool {
        self.scope.is_none() && self.queue.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use std::pin::pin;

    use assert_matches::assert_matches;
    use futures::channel::oneshot;
    use futures::stream;
    use futures_util::StreamExt;

    use crate::scope;
    use crate::stream::ScopedStreamExt;
    use crate::util::test::resolve;

    /// This tests that the results from futures submitted to
    /// scope_buffer_unordered can resolve out of order.
    #[tokio::test]
    async fn out_of_order() {
        let scope = scope!(|scope| {
            let (tx1, rx1) = oneshot::channel();
            let (tx2, rx2) = oneshot::channel();

            let mut stream = pin!(stream::iter(vec![rx1, rx2]).scope_buffer_unordered(2, &scope));

            // Futures can resolve out of order.
            tx2.send(1).unwrap();
            assert_eq!(resolve(stream.next()).await, Some(Some(Ok(1))));

            tx1.send(2).unwrap();
            assert_eq!(resolve(stream.next()).await, Some(Some(Ok(2))));

            assert_eq!(resolve(stream.next()).await, Some(None));
        });

        scope.await;
    }

    /// This test validates that futures outside of the buffered window are not
    /// executed until some space clears up in that window.
    #[tokio::test]
    async fn out_of_window() {
        let scope = scope!(|scope| {
            let (tx1, rx1) = oneshot::channel();
            let (tx2, rx2) = oneshot::channel();
            let (tx3, rx3) = oneshot::channel();

            let mut stream =
                pin!(stream::iter(vec![rx1, rx2, rx3]).scope_buffer_unordered(2, &scope));

            // rx3 is outside of the buffer so it should not resolve
            tx3.send(1).unwrap();
            assert_eq!(resolve(stream.next()).await, None);

            // Futures can resolve out of order.
            tx2.send(2).unwrap();
            assert_eq!(resolve(stream.next()).await, Some(Some(Ok(2))));

            // And now the newly added future can resolve too.
            assert_eq!(resolve(stream.next()).await, Some(Some(Ok(1))));

            tx1.send(3).unwrap();
            assert_eq!(resolve(stream.next()).await, Some(Some(Ok(3))));

            assert_eq!(resolve(stream.next()).await, Some(None));
        });

        scope.await;
    }

    /// This validates that futures within the buffer window are executed
    /// concurrently.
    #[tokio::test]
    async fn concurrent() {
        async fn pingpong(tx: oneshot::Sender<i32>, rx: oneshot::Receiver<i32>, ping: bool) -> i32 {
            if ping {
                tx.send(1).unwrap();
                rx.await.unwrap()
            } else {
                let value = rx.await.unwrap();
                tx.send(2).unwrap();
                value
            }
        }

        let scope = scope!(|scope| {
            let (tx1, rx1) = oneshot::channel();
            let (tx2, rx2) = oneshot::channel();

            // These tasks need to be run concurrently to complete.
            let tasks = vec![pingpong(tx1, rx2, true), pingpong(tx2, rx1, false)];
            let mut stream = pin!(stream::iter(tasks).scope_buffer_unordered(2, &scope));

            assert_matches!(resolve(stream.next()).await, Some(Some(_)));
            assert_matches!(resolve(stream.next()).await, Some(Some(_)));
        });

        scope.await;
    }
}
