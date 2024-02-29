//! Utilities for use _only_ in tests.

#![allow(dead_code)]

use std::future::Future;
use std::time::Duration;

/// Waits 2ms for the future to resolve.
///
/// This is enough time for the necessary chain of wakeups to occur while still
/// bailing quickly if the future isn't going to resolve immediately.
pub(crate) async fn resolve<F: Future>(future: F) -> Option<F::Output> {
    tokio::time::timeout(Duration::from_millis(2), future)
        .await
        .ok()
}
