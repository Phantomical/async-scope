//! Tests for various features.
//!
//! It is easier to have these as part of the library, and faster too.

use std::future::Future;
use std::time::Duration;

mod scope;
#[cfg(feature = "stream")]
mod stream;

async fn assert_does_not_hang<F: Future>(future: F) -> F::Output {
    match tokio::time::timeout(Duration::from_secs(1), future).await {
        Ok(result) => result,
        Err(_) => panic!("future timed out after 1s"),
    }
}
