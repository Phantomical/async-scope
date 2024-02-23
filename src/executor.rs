#![allow(dead_code)]

use std::future::Future;
use std::panic::{self, AssertUnwindSafe};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};

use atomic_waker::AtomicWaker;
use concurrent_queue::ConcurrentQueue;
use slotmap::SlotMap;

type FutureObj<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;
type TaskId = slotmap::DefaultKey;

struct Shared {
    waker: AtomicWaker,
    epoch: AtomicU64,
    queue: ConcurrentQueue<TaskId>,
    unhandled_panic: AtomicBool,
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
    spawn: Arc<ConcurrentQueue<FutureObj<'a>>>,

    peek: Option<TaskId>,
    max_polls_without_yield: u32,
    unhandled_panic: bool,
}

impl<'a> Executor<'a> {
    pub fn new() -> Self {
        let shared = Arc::new(Shared {
            waker: AtomicWaker::new(),
            epoch: AtomicU64::new(1),
            queue: ConcurrentQueue::unbounded(),
            unhandled_panic: AtomicBool::new(false),
        });
        let spawn = Arc::new(ConcurrentQueue::unbounded());

        Self {
            shared,
            tasks: SlotMap::new(),
            spawn,

            peek: None,
            max_polls_without_yield: 32,
            unhandled_panic: false,
        }
    }

    pub fn handle(&self) -> Handle<'a> {
        Handle {
            shared: self.shared.clone(),
            spawn: self.spawn.clone(),
        }
    }

    /// Set the maximum number of times we can poll subtasks before we
    /// unconditionally yield to the executor.
    pub fn max_polls_without_yield(&mut self, value: u32) {
        self.max_polls_without_yield = value;
    }

    pub fn unhandled_panic(&self) -> bool {
        self.unhandled_panic || self.shared.unhandled_panic.load(Ordering::Relaxed)
    }

    pub fn unhandled_panic_flag(&self) -> UnhandledPanicFlag {
        UnhandledPanicFlag(self.shared.clone())
    }

    fn spawn_epoch(&mut self, epoch: u64, future: FutureObj<'a>) -> TaskId {
        self.tasks.insert_with_key(|key| Task {
            waker: Arc::new(TaskWaker {
                epoch: AtomicU64::new(epoch),
                task: key,
                shared: self.shared.clone(),
            }),
            future,
            last_wake: 0,
        })
    }

    pub fn spawn(&mut self, future: FutureObj<'a>) {
        let taskid = self.spawn_epoch(self.shared.epoch.load(Ordering::Acquire), future);
        let _ = self.shared.queue.push(taskid);
    }

    fn next_task(&mut self, epoch: u64) -> Option<TaskId> {
        // Make sure to take the saved task before doing anything else.
        if let Some(taskid) = self.peek.take() {
            return Some(taskid);
        }

        // TODO: This could lead to starvation when there are lots of new tasks
        //       being added to the executor. This is probably not an issue
        //       under normal use though.
        if let Ok(task) = self.spawn.pop() {
            return Some(self.spawn_epoch(epoch, task));
        }

        self.shared.queue.pop().ok()
    }

    /// Poll all the tasks that have been added to the
    pub fn poll(&mut self, cx: &Context<'_>) -> Poll<()> {
        self.shared.waker.register(cx.waker());

        let epoch = self.shared.epoch.fetch_add(1, Ordering::AcqRel) + 1;
        let mut more_pending = true;

        for _ in 0..self.max_polls_without_yield {
            let taskid = match self.next_task(epoch) {
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
                Err(_payload) => {
                    self.tasks.remove(taskid);
                    self.unhandled_panic = true;
                }
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

    pub fn clear(&mut self) {
        // Prevent spawns and wakeups from accumulating
        self.spawn.close();
        self.shared.queue.close();

        // Drain and delete all existing tasks
        self.tasks.clear();
        while let Ok(_) = self.spawn.pop() {}
    }
}

impl<'a> Drop for Executor<'a> {
    fn drop(&mut self) {
        // Prevent spawns and wakeups from accumulating
        self.spawn.close();
        self.shared.queue.close();
    }
}

/// A handle to the executor that allows spawning new tasks onto the executor.
#[derive(Clone)]
pub(crate) struct Handle<'a> {
    spawn: Arc<ConcurrentQueue<FutureObj<'a>>>,
    shared: Arc<Shared>,
}

impl<'a> Handle<'a> {
    /// Spawn a new future onto the [`Executor`] for this handle.
    ///
    /// # Panics
    /// Panics if the executor has already been dropped.
    pub fn spawn(&self, future: FutureObj<'a>) {
        if self.spawn.push(future).is_err() {
            panic!("attempted to spawn a future on a dead AsyncScope")
        }

        self.shared.waker.wake();
    }

    pub fn unhandled_panic_flag(&self) -> UnhandledPanicFlag {
        UnhandledPanicFlag(self.shared.clone())
    }
}

pub(crate) struct UnhandledPanicFlag(Arc<Shared>);

impl UnhandledPanicFlag {
    pub fn mark_unhandled_panic(&self) {
        self.0.unhandled_panic.store(true, Ordering::Relaxed);
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

        let _ = self.shared.queue.push(self.task);

        // Prevent future wake calls from doing anything until the next time the scope
        // polls its tasks.
        self.epoch.store(current, Ordering::Release);
        self.shared.waker.wake();
    }
}
