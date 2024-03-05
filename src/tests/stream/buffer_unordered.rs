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

        let mut stream = pin!(stream::iter(vec![rx1, rx2]).scope_buffer_unordered(2, scope));

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

        let mut stream = pin!(stream::iter(vec![rx1, rx2, rx3]).scope_buffer_unordered(2, scope));

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
        let mut stream = pin!(stream::iter(tasks).scope_buffer_unordered(2, scope));

        assert_matches!(resolve(stream.next()).await, Some(Some(_)));
        assert_matches!(resolve(stream.next()).await, Some(Some(_)));
    });

    scope.await;
}
