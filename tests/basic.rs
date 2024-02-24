use std::panic::AssertUnwindSafe;
use std::time::Duration;

use async_scope::*;
use futures::FutureExt;

#[tokio::test]
async fn run_a_few() {
    let text = "test".to_string();

    scope!(|scope| {
        let scope = scope;

        let a = scope.spawn(async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            text.clone()
        });

        let b = scope.spawn(async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            text.clone()
        });

        a.await.unwrap();
        b.await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn run_in_macro() {
    let text = "test".to_string();

    scope!(|scope| {
        let a = scope.spawn(async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            text.clone()
        });

        let b = scope.spawn(async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            text.clone()
        });

        a.await.unwrap();
        b.await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn spawn_immediate() {
    use tokio::time::timeout;

    scope!(|scope| {
        let a = scope.spawn(async {});
        let b = scope.spawn(async {});

        timeout(Duration::from_millis(100), a)
            .await
            .expect("JoinHandle for task A timed out")
            .unwrap();

        timeout(Duration::from_millis(100), b)
            .await
            .expect("JoinHandle for task A timed out")
            .unwrap();
    })
    .await;
}

#[tokio::test]
async fn spawn_immediate_2() {
    let data = "some data to be borrowed".to_string();

    let scope = scope!(|scope| {
        // Some tasks which borrow data
        let task_a = scope.spawn(async { data.clone() });
        let task_b = scope.spawn(async { data.clone() });

        task_a.await.unwrap();
        task_b.await.unwrap();
    });

    tokio::time::timeout(Duration::from_millis(100), scope)
        .await
        .expect("AsyncScope hung");
}

#[tokio::test]
async fn spawn_many() {
    let scope = scope!(|scope| for _ in 0..1024 {
        scope.spawn(async { tokio::time::sleep(Duration::from_millis(20)).await });
    });

    tokio::time::timeout(Duration::from_millis(40), scope)
        .await
        .expect("AsyncScope took too long to run");
}

#[tokio::test]
async fn spawn_many_immediate() {
    let scope = scope!(|scope| for _ in 0..1024 {
        scope.spawn(async {});
    });

    scope.await;
}

#[tokio::test]
async fn spawn_and_cancel() {
    let scope = scope!(|scope| {
        let handle = scope.spawn(async { tokio::time::sleep(Duration::from_millis(100)).await });
        tokio::time::sleep(Duration::from_millis(5)).await;

        handle.abort();
        let result = handle
            .await
            .expect_err("cancelled future completed successfully?");

        assert!(result.is_cancelled());
    });

    scope.await;
}

#[tokio::test]
async fn unhandled_panic_in_subtask() {
    let mut main_task_completed = false;
    let scope = scope!(|scope| {
        scope.spawn(async {
            panic!();
        });

        tokio::time::sleep(Duration::from_millis(10)).await;

        main_task_completed = true;
    });

    let result = AssertUnwindSafe(scope).catch_unwind().await;

    assert!(result.is_err());
    assert!(main_task_completed);
}
