use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::task::{Context, Poll, Wake, Waker};

use futures::task::AtomicWaker;
use futures::FutureExt;
use slotmap::SlotMap;

use crate::Options;

type FutureObj<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;
type TaskId = slotmap::DefaultKey;

struct Shared {
    waker: AtomicWaker,
    epoch: AtomicU64,
    queue: mpsc::Sender<TaskId>,
    options: Options,
}

struct Task<'a> {
    waker: Arc<TaskWaker>,
    future: FutureObj<'a>,

    /// The epoch at which this future was last woken.
    last_wake: u64,
}

/// A simple local executor.
pub(crate) struct Executor<'a> {
    shared: Arc<Shared>,
    tasks: SlotMap<TaskId, Task<'a>>,

    run_queue: mpsc::Receiver<TaskId>,
    spawn_queue: mpsc::Receiver<FutureObj<'a>>,
}

impl<'a> Executor<'a> {
    pub fn new(options: Options) -> (Self, Handle<'a>) {
        let (run_tx, run_rx) = mpsc::channel();
        let (spawn_tx, spawn_rx) = mpsc::channel();
        let shared = Arc::new(Shared {
            waker: AtomicWaker::new(),
            epoch: AtomicU64::new(0),
            queue: run_tx,
            options,
        });

        (
            Self {
                shared: shared.clone(),
                tasks: SlotMap::new(),

                run_queue: run_rx,
                spawn_queue: spawn_rx,
            },
            Handle {
                spawn: spawn_tx,
                shared,
            },
        )
    }

    pub fn options(&self) -> &Options {
        &self.shared.options
    }

    fn spawn_epoch(&mut self, epoch: u64, future: FutureObj<'a>) {
        let taskid = self.tasks.insert_with_key(|key| Task {
            waker: Arc::new(TaskWaker {
                epoch: AtomicU64::new(epoch),
                task: key,
                shared: self.shared.clone(),
            }),
            future,
            last_wake: 0,
        });

        let _ = self.shared.queue.send(taskid);
    }

    pub fn spawn(&mut self, future: FutureObj<'a>) {
        self.spawn_epoch(self.shared.epoch.load(Ordering::Acquire), future)
    }

    pub fn poll(&mut self, cx: &Context<'_>) -> Poll<()> {
        self.shared.waker.register(cx.waker());
        let epoch = self.shared.epoch.fetch_add(1, Ordering::AcqRel) + 1;

        while let Ok(future) = self.spawn_queue.try_recv() {
            self.spawn_epoch(epoch, future);
        }

        loop {
            let taskid = match self.run_queue.try_recv() {
                Ok(taskid) => taskid,
                Err(_) => break,
            };

            let task = match self.tasks.get_mut(taskid) {
                Some(task) => task,
                None => continue,
            };

            // Avoid waking the future twice within the same poll.
            if task.last_wake != epoch {
                let waker = Waker::from(task.waker.clone());
                let mut cx = Context::from_waker(&waker);

                task.last_wake = epoch;
                if task.future.poll_unpin(&mut cx).is_ready() {
                    self.tasks.remove(taskid);
                }
            }

            while let Ok(future) = self.spawn_queue.try_recv() {
                self.spawn_epoch(epoch, future);
            }
        }

        if self.tasks.is_empty() {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}

#[derive(Clone)]
pub(crate) struct Handle<'a> {
    spawn: mpsc::Sender<FutureObj<'a>>,
    shared: Arc<Shared>,
}

impl<'a> Handle<'a> {
    pub fn spawn(&self, future: FutureObj<'a>) {
        if self.spawn.send(future).is_err() {
            panic!("attempted to spawn a future on a dead AsyncScope")
        }

        self.shared.waker.wake();
    }

    pub fn options(&self) -> &Options {
        &self.shared.options
    }
}

struct TaskWaker {
    epoch: AtomicU64,
    task: TaskId,
    shared: Arc<Shared>,
}

impl Wake for TaskWaker {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref()
    }

    fn wake_by_ref(self: &Arc<Self>) {
        let current = self.shared.epoch.load(Ordering::Acquire);
        let epoch = self.epoch.load(Ordering::Acquire);

        if current <= epoch {
            // We have already registered a wake notification for the current epoch, don't
            // enqueue anymore to avoid blowing out the queue.
            return;
        }

        let _ = self.shared.queue.send(self.task);

        // Prevent future wake calls from doing anything until the next time the scope
        // polls its tasks.
        self.epoch.store(current, Ordering::Release);
        self.shared.waker.wake();
    }
}
