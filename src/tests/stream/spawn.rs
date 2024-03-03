use std::panic::AssertUnwindSafe;

use futures::{FutureExt, StreamExt};

use crate::stream::ScopedStreamExt;

/// This test validates that panics are emitted at the proper point in the
/// stream and not before.
#[tokio::test]
async fn panic_after_n() {
    let scope = scope!(|scope| {
        let mut stream = futures::stream::iter(0..10)
            .map(|index| match index {
                3 => panic!(),
                _ => (),
            })
            .scope_spawn(4, &scope);

        assert!(stream.next().await.is_some());
        assert!(stream.next().await.is_some());
        assert!(stream.next().await.is_some());

        AssertUnwindSafe(stream.next())
            .catch_unwind()
            .await
            .expect_err("stream did not propagate panic");
    });

    scope.await
}
