use std::future::{pending, Future};
use std::panic::AssertUnwindSafe;

use futures_util::FutureExt;
use tokio::sync::Barrier;
use tokio::task::yield_now;

use super::assert_does_not_hang;
use crate::{scope, ScopeHandle};

/// This test ensures that the spawned tests are actually run in parallel.
#[tokio::test]
async fn tasks_run_in_parallel() {
    let barrier = Barrier::new(2);

    let scope = scope!(|scope| {
        let a = scope.spawn(async { barrier.wait().await });
        let b = scope.spawn(async { barrier.wait().await });

        a.await.unwrap();
        b.await.unwrap();
    });

    assert_does_not_hang(scope).await;
}

/// Same as above but with a large number of tasks.
#[tokio::test]
async fn tasks_run_in_parallel_many() {
    let taskcount = 1024;
    let barrier = Barrier::new(taskcount);

    let scope = scope!(|scope| {
        for _ in 0..taskcount {
            scope.spawn(async { barrier.wait().await });
        }
    });

    assert_does_not_hang(scope).await;
}

/// Spawn tasks immediately, during the first poll.
#[tokio::test]
async fn spawn_immediate() {
    scope!(|scope| {
        let a = scope.spawn(async {});
        let b = scope.spawn(async {});

        assert_does_not_hang(a).await.unwrap();
        assert_does_not_hang(b).await.unwrap();
    })
    .await;
}

/// Spawn new tasks after the main task has completed.
///
/// Here no tasks are actually spawned until the main task has completed. That
/// repeats for each task in the chain: there is nothing running on the executor
/// when it is spawned.
#[tokio::test]
async fn spawn_late() {
    fn mktask<'scope, F>(
        scope: &ScopeHandle<'scope>,
        inner: F,
    ) -> impl Future<Output = ()> + Send + 'scope
    where
        F: Future<Output = ()> + Send + 'scope,
    {
        let scope = scope.clone();

        async move {
            scope.spawn(inner);
        }
    }

    let scope = scope!(|scope| {
        let task = async {};
        let task = mktask(&scope, task);
        let task = mktask(&scope, task);
        let task = mktask(&scope, task);
        let task = mktask(&scope, task);
        let task = mktask(&scope, task);
        let task = mktask(&scope, task);
        let task = mktask(&scope, task);

        scope.spawn(task);
    });

    assert_does_not_hang(scope).await;
}

/// This verifies that cancelling a task that has no pending poll notifications
/// works as expected.
#[tokio::test]
async fn spawn_and_cancel() {
    let scope = scope!(|scope| {
        let handle = scope.spawn(pending::<()>());

        // Ensure the new tasks gets spawned and polled at least once
        yield_now().await;
        yield_now().await;

        handle.abort();

        let result = handle
            .await
            .expect_err("cancelled task completed successfully");
        assert!(result.is_cancelled());
    });

    assert_does_not_hang(scope).await;
}

/// This verifies that cancelling a task that has no pending poll notifications
/// works as expected.
#[tokio::test]
async fn spawn_and_cancel_immediate() {
    let scope = scope!(|scope| {
        let handle = scope.spawn(pending::<()>());

        handle.abort();

        let result = handle
            .await
            .expect_err("cancelled task completed successfully");
        assert!(result.is_cancelled());
    });

    assert_does_not_hang(scope).await;
}

/// This verifies that the panic for subtask does not propagate when explicitly
/// handled by consuming its JoinHandle.
#[tokio::test]
async fn handled_subtask_panic() {
    let scope = scope!(|scope| {
        let handle = scope.spawn(async {
            panic!();
        });

        let result = handle
            .await
            .expect_err("panicking task completed successfully");
        assert!(result.is_panic());
    });

    assert_does_not_hang(scope).await;
}

/// A unique type that we can downcast the panic to.
struct DummyPanic;

/// Verify that the JoinHandle error contains the very same error that the task
/// panicked with.
#[tokio::test]
async fn handled_subtask_panic_propagates_payload() {
    let scope = scope!(|scope| {
        let handle = scope.spawn(async {
            std::panic::panic_any(DummyPanic);
        });

        let result = handle
            .await
            .expect_err("panicking task completed successfully");

        assert!(result.is_panic());
        let payload = result.into_panic();

        assert!(payload.is::<DummyPanic>());
    });

    assert_does_not_hang(scope).await;
}

/// This validates that the main tasks still runs to completion even if a
/// subtask panics.
///
/// It also verifies that the scope panics if a subtask panics and that panic is
/// unhandled.
#[tokio::test]
async fn unhandled_subtask_panic() {
    let mut main_task_completed = false;
    let scope = scope!(|scope| {
        scope.spawn(async { panic!() });

        // Ensure the subtask is polled and has a chance to panic.
        yield_now().await;
        yield_now().await;

        main_task_completed = true;
    });

    let result = AssertUnwindSafe(scope).catch_unwind().await;

    assert!(result.is_err());
    assert!(main_task_completed);
}

/// This test validates that the actual panic payload is propagated through when
/// there is an unhandled panic in a subtask.
#[tokio::test]
async fn unhandled_subtask_panic_propagates_payload() {
    let scope = scope!(|scope| {
        scope.spawn(async { std::panic::panic_any(DummyPanic) });

        // Ensure the subtask is polled and has a chance to panic.
        yield_now().await;
        yield_now().await;
    });

    let error = AssertUnwindSafe(scope)
        .catch_unwind()
        .await
        .expect_err("scope did not panic");

    assert!(error.is::<DummyPanic>());
}
