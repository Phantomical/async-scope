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
