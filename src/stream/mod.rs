//! Extensions for working with streams within scoped tasks.
//!
//! All the extension methods on the [`ScopedStreamExt`] trait are variants of
//! those on [`futures::StreamExt`][0] trait which spawn their inner futures
//! onto the surrounding scope.
//!
//! This prevents a somewhat common issue when using certain stream combinators
//! in a loop (notably, the `buffered` combinator). See the code below:
//! ```
//! # #[tokio::main(flavor = "current_thread")]
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
//!     # break;
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

use std::future::Future;

use futures_core::Stream;

use crate::{AsyncScope, ScopeHandle};

used_in_docs!(AsyncScope);

mod buffer_unordered;
mod buffered;
mod spawn;

pub use self::buffer_unordered::BufferUnordered;
pub use self::buffered::Buffered;
pub use self::spawn::Spawn;

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
/// # #[tokio::main(flavor = "current_thread")]
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
/// #   /* <- optimization to make doctests run a bit faster
///     tokio::time::sleep(Duration::from_millis(50)).await;
/// #   */
/// }
/// # }
/// ```
///
/// The methods on this trait prevent this by running the futures on the
/// surrounding scope. This allows them to run concurrently with the execution
/// of the current task, which prevents timeouts or disconnects resulting from
/// the stream not being polled during the loop body.
///
/// [`StreamExt`]: https://docs.rs/futures/latest/futures/stream/trait.StreamExt.html
pub trait ScopedStreamExt: Stream {
    /// An adaptor for creating a buffered list of pending futures.
    ///
    /// The stream's items will be spawned onto the provided scope. Up to `n`
    /// futures will be allowed to execute concurrently. Their outputs will be
    /// buffered and returned in the same order as the underlying stream. No
    /// more than `n` futures will be buffered at any point in time, and less
    /// than `n` may be buffered depending on the state of each future.
    ///
    /// The returned stream will be a stream of each future's output.
    ///
    /// # Cancellation
    /// If the stream is dropped then all buffered tasks will be cancelled.
    ///
    /// # Panics
    /// If any of the spawned futures panic then that panic will be raised when
    /// the future's output would have been returned from the stream.
    ///
    /// If a task panics but the stream is dropped before its output would have
    /// been returned then that will count as an unhandled panic to the outer
    /// [`AsyncScope`] and it will panic on scope exit.
    fn scope_buffered<'scope>(self, n: usize, scope: &ScopeHandle<'scope>) -> Buffered<'scope, Self>
    where
        Self: Sized,
        Self::Item: Future + Send + 'scope,
        <Self::Item as Future>::Output: Send + 'scope,
    {
        Buffered::new(self, n, scope)
    }

    /// An adaptor for creating a unordered buffered list of pending futures.
    ///
    /// The stream's items will be spawned onto the provided scope. Up to `n`
    /// futures will be allowed to execute concurrently. Their outputs will be
    /// returned in the order in which the futures complete. No more than `n`
    /// futures will be buffered at any point in time, and less than `n` may be
    /// buffered depending on the state of each future.
    ///
    /// The returned stream will be a stream of each future's output.
    ///
    /// # Cancellation
    /// If the stream is dropped then all buffered tasks will be cancelled.
    ///
    /// # Panics
    /// If any of the spawned futures panic then that panic will be raised when
    /// the future's output would have been returned from the stream.
    ///
    /// If a task panics but the stream is dropped before its output would have
    /// been returned then that will count as an unhandled panic to the outer
    /// [`AsyncScope`] and it will panic on scope exit.
    ///
    /// # Examples
    /// ```
    /// # #[tokio::main(flavor = "current_thread")]
    /// # async fn main() -> Result<(), i32> {
    /// use futures::channel::oneshot;
    /// use futures::stream::{self, StreamExt};
    ///
    /// let (send_one, recv_one) = oneshot::channel();
    /// let (send_two, recv_two) = oneshot::channel();
    ///
    /// async_scope::scope!(|scope| {
    ///     let mut buffered = stream::iter(vec![recv_one, recv_two])
    ///         .buffer_unordered(4);
    ///
    ///     send_two.send(2i32)?;
    ///     assert_eq!(buffered.next().await, Some(Ok(2i32)));
    ///
    ///     send_one.send(1i32)?;
    ///     assert_eq!(buffered.next().await, Some(Ok(1i32)));
    ///     # Ok(())
    /// })
    /// .await
    /// # }
    /// ```
    fn scope_buffer_unordered<'scope>(
        self,
        n: usize,
        scope: &ScopeHandle<'scope>,
    ) -> BufferUnordered<'scope, Self>
    where
        Self: Sized,
        Self::Item: Future + Send + 'scope,
        <Self::Item as Future>::Output: Send + 'scope,
    {
        BufferUnordered::new(self, n, scope)
    }

    /// An adaptor that spawns the stream as task on the parent scope.
    ///
    /// The stream will start being polled immediately. Up to `buffer` items
    /// output by the stream will be buffered before stream task will block.
    /// Setting `buffer` to 0 will result in the stream task synchronizing with
    /// the `poll_next` call on the [`Spawn`] stream. The stream and the loop
    /// body will still be able to execute concurrently.
    ///
    /// Note that unlike [`scope_buffered`][0] and [`scope_buffer_unordered`][1]
    /// this does not make the stream evaluate multiple items concurrently.
    ///
    /// # Cancellation
    /// Dropping the returned [`Spawn`] stream will abort the spawned task.
    ///
    /// # Panics
    /// If the spawned stream task panics then that panic will be raised here
    /// once all buffered items have been produced. If the stream is dropped
    /// before the panic is thrown then that will count as an unhandled panic
    /// and the parent [`AsyncScope`] will panic once the main task exits.
    ///
    /// [0]: ScopedStreamExt::scope_buffered
    /// [1]: ScopedStreamExt::scope_buffer_unordered
    fn scope_spawn<'scope>(self, buffer: usize, scope: &ScopeHandle<'scope>) -> Spawn<'scope, Self>
    where
        Self: Send + Sized + 'scope,
        Self::Item: Send + 'scope,
    {
        Spawn::new(self, buffer, scope)
    }
}

impl<S: Stream> ScopedStreamExt for S {}
