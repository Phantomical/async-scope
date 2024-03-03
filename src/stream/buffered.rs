use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures_core::{FusedStream, Stream};
use pin_project_lite::pin_project;

use crate::{JoinHandle, ScopeHandle};

pin_project! {
    /// Stream for the [`scope_buffered`] method.
    pub struct Buffered<'scope, S>
    where
        S: Stream,
        S::Item: Future,
    {
        #[pin]
        stream: S,
        limit: usize,
        queue: VecDeque<JoinHandle<'scope, <S::Item as Future>::Output>>,

        // scope = None means that the stream has been polled to completion
        scope: Option<ScopeHandle<'scope>>,
    }

    impl<'scope, S> PinnedDrop for Buffered<'scope, S>
    where
        S: Stream,
        S::Item: Future,
    {
        fn drop(this: Pin<&mut Self>) {
            let this = this.project();

            for handle in std::mem::take(this.queue) {
                handle.abort();
            }
        }
    }
}

impl<'scope, S> Buffered<'scope, S>
where
    S: Stream,
    S::Item: Future + Send + 'scope,
    <S::Item as Future>::Output: Send + 'scope,
{
    pub(crate) fn new(stream: S, n: usize, scope: &ScopeHandle<'scope>) -> Self {
        Buffered {
            stream,
            limit: n,
            queue: VecDeque::with_capacity(n),
            scope: Some(scope.clone()),
        }
    }
}

impl<'scope, S> Stream for Buffered<'scope, S>
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
                    Poll::Ready(Some(future)) => this.queue.push_back(scope.spawn(future)),
                    Poll::Ready(None) => {
                        *this.scope = None;
                        break;
                    }
                    Poll::Pending => break,
                }
            }
        }

        let handle = match this.queue.front_mut() {
            Some(handle) => Pin::new(handle),
            None => return Poll::Ready(None),
        };

        match handle.poll(cx) {
            Poll::Ready(result) => {
                this.queue.pop_front();

                match result {
                    Ok(value) => Poll::Ready(Some(value)),
                    Err(error) => match error.try_into_panic() {
                        Ok(payload) => std::panic::resume_unwind(payload),
                        Err(_) => panic!("a task within the buffer was cancelled"),
                    },
                }
            }
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

impl<'scope, S> FusedStream for Buffered<'scope, S>
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

    #[tokio::test]
    async fn out_of_order() {
        let scope = scope!(|scope| {
            let (tx1, rx1) = oneshot::channel();
            let (tx2, rx2) = oneshot::channel();

            let mut stream = pin!(stream::iter(vec![rx1, rx2]).scope_buffered(2, &scope));

            tx2.send(1).unwrap();

            // The second future has resolved but that shouldn't result in the stream
            // returning a value.
            assert_eq!(resolve(stream.next()).await, None);

            tx1.send(2).unwrap();

            assert_eq!(resolve(stream.next()).await, Some(Some(Ok(2))));
            assert_eq!(resolve(stream.next()).await, Some(Some(Ok(1))));
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
            let mut stream = pin!(stream::iter(tasks).scope_buffered(2, &scope));

            assert_matches!(resolve(stream.next()).await, Some(Some(_)));
            assert_matches!(resolve(stream.next()).await, Some(Some(_)));
        });

        scope.await;
    }
}
