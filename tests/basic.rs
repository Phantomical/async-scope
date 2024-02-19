use std::time::Duration;

use async_scope::*;

#[tokio::test]
async fn run_a_few() {
    let text = "test".to_string();

    scope(|scope| async {
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
