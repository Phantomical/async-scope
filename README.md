# async-scope

An async version of `std::thread::scope`.

This crate allows you to write futures that use `spawn` but also borrow data
from the surrounding scope. This is done by running individual tasks using a
local executor within the current task.

## Example

```rust
use std::time::Duration;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut a = vec![1, 2, 3];
    let mut x = 0;

    let scope = async_scope::scope!(|scope| {
        scope.spawn(async {
            // We can borrow `a` here
            dbg!(&a);
        });

        scope.spawn(async {
            // We can even mutably borrow `x` here.
            x += a[0] + a[2];
        });

        let handle = scope.spawn(async {
            // We can also run arbitrary futures as part of the scope tasks.
            tokio::time::sleep(Duration::from_millis(50)).await;
        });

        // The main task is also async and can await on futures.
        tokio::time::sleep(Duration::from_millis(10)).await;

        // We can even wait for tasks that have been spawned.
        handle
            .await
            .expect("the task panicked");
    });

    // We do need to await the scope so that it can run tasks, though.
    scope.await;
}
```

## See Also
- If you are content with requiring individual tasks to be `'static` then you
  can use `tokio`'s [`JoinSet`][tokio-joinset].
- If all your futures have the same type you can use
  [`FuturesUnordered`][futures-unordered] from the `futures-util` crate.
- If you can guarantee that the current future will never be forgotten then you
  can use the [`async_scoped`][async-scoped] crate.

[async-scoped]: https://docs.rs/async-scoped
[tokio-joinset]: https://docs.rs/tokio/1/tokio/task/struct.JoinSet.html
[futures-unordered]: https://docs.rs/futures-util/0.3/futures_util/stream/futures_unordered/struct.FuturesUnordered.html
