//! Async equivalent of [`std::thread::scope`].
//!
//! This crate allows you to write futures that use `spawn` but also borrow data
//! from the current function. It does this by running those futures in a local
//! executor within the current task.
//!
//! # Example
//! ```
//! # #[tokio::main]
//! # async fn main() {
//! use std::time::Duration;
//!
//! let mut a = vec![1, 2, 3];
//! let mut x = 0;
//!
//! let scope = async_scope::scope!(|scope| {
//!     scope.spawn(async {
//!         // We can borrow `a` here
//!         dbg!(&a);
//!     });
//!
//!     scope.spawn(async {
//!         // We can even mutably borrow `x` here because no other threads are
//!         // using it.
//!         x += a[0] + a[2];
//!     });
//!
//!     let handle = scope.spawn(async {
//!         // We can also run arbitrary futures as part of the scope tasks.
//!         tokio::time::sleep(Duration::from_millis(50)).await;
//!     });
//!
//!     // The main task can also await on futures
//!     tokio::time::sleep(Duration::from_millis(10)).await;
//!
//!     // and even wait for tasks that have been spawned
//!     handle
//!         .await
//!         .expect("the task panicked");
//! });
//!
//! // We do need to await the scope so that it can run the tasks, though.
//! scope.await;
//! # }
//! ```

#![deny(unsafe_code)]
// #![warn(missing_docs)]

/// Helper macro used to silence `unused_import` warnings when an item is
/// only imported in order to refer to it within a doc comment.
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

mod error;
mod executor;
mod scope;
mod util;
mod wrapper;

pub use crate::error::JoinError;
pub use crate::scope::{AbortHandle, AsyncScope, JoinHandle, ScopeHandle};

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
/// # #[tokio::main]
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
    (|$scope:ident| $body:expr) => {
        $crate::AsyncScope::new(|$scope| async {
            let $scope = $scope;

            $body
        })
    };
}
