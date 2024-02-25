//! Extensions for working with streams within scoped tasks.
//!
//! All the extension methods on the [`ScopedStreamExt`] trait are variants of
//! those on [`futures::StreamExt`][0] trait which spawn their inner futures
//! onto the surrounding scope.
//!
//! This prevents a somewhat common issue when using certain stream combinators
//! in a loop (notably, the `buffered` combinator). See the code below:
//! ```
//! # #[tokio::main]
//! # async fn main() {
//! # use std::time::Duration;
//! use futures::StreamExt;
//! use futures::stream::repeat_with;
//!
//! let stream = repeat_with(|| tokio::time::sleep(Duration::from_millis(20)))
//!         .take(50)
//!         .buffered(10);
//! let mut stream = std::pin::pin!(stream);
//!
//! while let Some(_) = stream.next().await {
//!     // All futures in the stream are paused while the work in the loop here
//!     // is running. Not so bad in this case, but can be much worse when the
//!     // work takes longer.
//!     tokio::time::sleep(Duration::from_millis(50)).await;
//! }
//! # }
//! ```
//! When in the body of the while loop, all the futures within the stream are
//! suspended. Since we are currently awaiting the `sleep` in the loop, the
//! stream does not get polled. In this example, that's not so bad, but when
//! there is more work going on in the loop the futures in the stream may find
//! themselves timing out, having network requests disconnect, or other
//! undesirable effects.
//!
//! By using the combinators in [`ScopedStreamExt`] the futures are polled by
//! the scope itself and so they can run concurrently with the loop itself.
//!
//! # Examples
//! TODO
//!
//! [0]: https://docs.rs/futures/latest/futures/stream/trait.StreamExt.html

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures_core::Stream;
use pin_project_lite::pin_project;

use crate::{JoinHandle, ScopeHandle};

/// An extension trait for [`Stream`]s that provides combinators functions that
/// take advantage of a surrounding scope.
///
/// All of the functions on this trait match an existing function on the
/// [`StreamExt`] trait within the futures crate but they spawn their tasks on
/// the surrounding scope via a [`ScopeHandle`] instead of running an internal
/// executor.
///
/// This prevents a common issue with streams where one has a for loop that
/// looks like this:
/// ```
/// # #[tokio::main]
/// # async fn main() {
/// # use std::time::Duration;
/// use futures::StreamExt;
/// use futures::stream::repeat_with;
///
/// let stream = repeat_with(|| tokio::time::sleep(Duration::from_millis(20)))
///         .take(50)
///         .buffered(10);
/// let mut stream = std::pin::pin!(stream);
///
/// while let Some(_) = stream.next().await {
///     // All futures in the stream are paused while the work in the loop here
///     // is running. Not so bad in this case, but can be much worse when the
///     // work takes longer.
///     tokio::time::sleep(Duration::from_millis(50)).await;
/// }
/// # }
/// ```
///
/// The methods on this trait prevent this by running the futures on the
/// surrounding scope. This allows them to run concurrently with the execution
/// of the current task, which prevents timeouts or disconnects resulting from
///
/// [`StreamExt`]: https://docs.rs/futures/latest/futures/stream/trait.StreamExt.html
pub trait ScopedStreamExt: Stream {
    fn scope_buffered<'scope>(self, n: usize, scope: &ScopeHandle<'scope>) -> Buffered<'scope, Self>
    where
        Self: Sized,
        Self::Item: Future + Send + 'scope,
        <Self::Item as Future>::Output: Send + 'scope,
    {
        Buffered {
            stream: self,
            limit: n,
            futures: VecDeque::with_capacity(n),
            scope: Some(scope.clone()),
        }
    }
}

impl<S: Stream> ScopedStreamExt for S {}

pin_project! {
    pub struct Buffered<'scope, S>
    where
        S: Stream,
        S::Item: Future,
    {
        #[pin]
        stream: S,
        limit: usize,
        futures: VecDeque<JoinHandle<'scope, <S::Item as Future>::Output>>,

        // scope = None means that the stream has been polled to completion
        scope: Option<ScopeHandle<'scope>>,
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
            while this.futures.len() < *this.limit {
                match this.stream.as_mut().poll_next(cx) {
                    Poll::Ready(Some(future)) => this.futures.push_back(scope.spawn(future)),
                    Poll::Ready(None) => {
                        *this.scope = None;
                        break;
                    }
                    Poll::Pending => break,
                }
            }
        }

        let handle = match this.futures.front_mut() {
            Some(handle) => Pin::new(handle),
            None => return Poll::Ready(None),
        };

        match handle.poll(cx) {
            Poll::Ready(Ok(value)) => Poll::Ready(Some(value)),
            Poll::Ready(Err(error)) => match error.try_into_panic() {
                Ok(payload) => std::panic::resume_unwind(payload),
                Err(_) => panic!("a task within the buffer was cancelled"),
            },
            Poll::Pending => Poll::Pending,
        }
    }
}
