use crate::{scope, AsyncScope, JoinHandle, ScopeHandle};

used_in_docs!(scope, AsyncScope, JoinHandle, ScopeHandle);

/// Configuration options for [`AsyncScope`].
#[derive(Clone, Debug)]
pub struct Options {
    pub(crate) catch_unwind: bool,
    pub(crate) cancel_remaining_tasks_on_exit: bool,
    pub(crate) max_polls_without_yield: u32,
}

impl Options {
    /// Create a new set of `Options` with the default configuration.
    pub fn new() -> Self {
        Self {
            catch_unwind: true,
            cancel_remaining_tasks_on_exit: false,
            max_polls_without_yield: 128,
        }
    }

    /// Set the maximum number of futures that can be polled before yielding to
    /// the parent executor.
    ///
    /// Under high load it is possible for a call to [`AsyncScope::poll`] to
    /// block for a significant amount of time, potentially indefinitely if new
    /// tasks are spawned or woken up faster than they can be polled. To the
    /// runtime, this looks as if the task has hung. We resolve this by
    /// unconditionally yielding back to the executor after having polled some
    /// number of tasks. This option controls how often we do so.
    ///
    /// The default value is 128.
    ///
    /// # Panics
    /// Panics if `value` is 0.
    ///
    /// [`AsyncScope::poll`]: struct.AsyncScope.html#method.poll
    pub fn max_polls_without_yield(mut self, value: u32) -> Self {
        assert_ne!(value, 0);

        self.max_polls_without_yield = value;
        self
    }

    /// Configure whether panics that unwind out of a task should be caught or
    /// allowed to propagate up out of the [`AsyncScope`] itself.
    ///
    /// By default, panics are caught and can be accessed by awaiting the
    /// [`JoinHandle`] returned from [`ScopeHandle::spawn`]. This matches the
    /// existing behaviour of [`std::thread::spawn`] and [`tokio`].
    ///
    /// Setting this to `false` means that panics will be allowed to unwind
    /// directly out of the executor with no chance for the main scope task to
    /// handle them.
    ///
    /// Note that panics from the inital scope task (i.e. the one passed in to
    /// [`scope`]) are automatically propagated out from `AsyncScope::poll`.
    ///
    /// [`tokio`]: https://docs.rs/tokio
    pub fn catch_unwind(mut self, enabled: bool) -> Self {
        self.catch_unwind = enabled;
        self
    }

    /// Whether remaining tasks should be polled to completion after the main
    /// scope task exits or just dropped.
    ///
    /// By default, whatever tasks are left in the [`AsyncScope`] continues to
    /// run until all tasks within have been polled to completion. This matches
    /// the existing behaviour of [`std::thread::scope`].
    ///
    /// Setting this to `true` means that once the initial scope task (i.e. the
    /// one passed in to [`scope`]) completes then all other tasks will be
    /// dropped without them being polled to completion.
    pub fn cancel_remaining_tasks_on_exit(mut self, enabled: bool) -> Self {
        self.cancel_remaining_tasks_on_exit = enabled;
        self
    }
}

impl Default for Options {
    fn default() -> Self {
        Self::new()
    }
}
