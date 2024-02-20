use std::future::Future;
use std::panic::{self, AssertUnwindSafe};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::task::{Context, Poll, Wake, Waker};

use atomic_waker::AtomicWaker;
use slotmap::SlotMap;

use crate::error::Payload;
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

    peek: Option<TaskId>,
    run_queue: mpsc::Receiver<TaskId>,
    spawn_queue: mpsc::Receiver<FutureObj<'a>>,
    uncaught_panic: Option<Payload>,
    max_polls_without_yield: u32,
}

impl<'a> Executor<'a> {
    pub fn new(options: Options) -> (Self, Handle<'a>) {
        let (run_tx, run_rx) = mpsc::channel();
        let (spawn_tx, spawn_rx) = mpsc::channel();
        let shared = Arc::new(Shared {
            waker: AtomicWaker::new(),
            epoch: AtomicU64::new(1),
            queue: run_tx,
            options,
        });

        (
            Self {
                shared: shared.clone(),
                tasks: SlotMap::new(),

                peek: None,
                run_queue: run_rx,
                spawn_queue: spawn_rx,
                uncaught_panic: None,
                max_polls_without_yield: 32,
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

    fn next_task(&mut self) -> Option<TaskId> {
        if let Some(taskid) = self.peek.take() {
            return Some(taskid);
        }

        self.run_queue.try_recv().ok()
    }

    pub fn poll(&mut self, cx: &Context<'_>) -> Poll<()> {
        self.shared.waker.register(cx.waker());

        let epoch = self.shared.epoch.fetch_add(1, Ordering::AcqRel) + 1;
        let mut more_pending = true;

        while let Ok(future) = self.spawn_queue.try_recv() {
            self.spawn_epoch(epoch, future);
        }

        for _ in 0..self.max_polls_without_yield {
            let taskid = match self.next_task() {
                Some(taskid) => taskid,
                None => {
                    more_pending = false;
                    break;
                }
            };

            let task = match self.tasks.get_mut(taskid) {
                Some(task) => task,
                None => continue,
            };

            // Avoid waking the same future twice within the same poll.
            //
            // Tokio has a feature where, after a future has done enough work, various tokio
            // futures will start returning Pending even if they might be ready. This makes
            // it so other futures on the executor get a chance to run.
            //
            // However, this all falls apart if a local executor (like Executor!) continues
            // to poll them repeatedly. It can get even worse if you end up with multiple
            // executors within each other because then the call repetitions multiply.
            if task.last_wake == epoch {
                // We still need to process this wakeup though, so save it for the next time.
                self.peek = Some(taskid);
                break;
            }

            let waker = Waker::from(task.waker.clone());
            let mut cx = Context::from_waker(&waker);

            task.last_wake = epoch;
            let result =
                panic::catch_unwind(AssertUnwindSafe(|| task.future.as_mut().poll(&mut cx)));

            match result {
                Ok(Poll::Pending) => (),
                Ok(Poll::Ready(())) => {
                    self.tasks.remove(taskid);
                }
                Err(payload) => {
                    self.tasks.remove(taskid);
                    self.uncaught_panic = Some(payload);
                }
            }

            while let Ok(future) = self.spawn_queue.try_recv() {
                self.spawn_epoch(epoch, future);
            }
        }

        // If we exited due to running out of attempts or encountering a task that was
        // already woken this poll then we need to tell the executor to wake us back up
        // immediately so we can keep working through the queue.
        if more_pending {
            cx.waker().wake_by_ref();
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
