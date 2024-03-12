use std::future::Future;
use std::sync::Mutex;
use std::task::{Context, Poll};

use crate::error::Payload;
use crate::executor::Executor;
use crate::util::variance::{Covariant, Invariant};
use crate::wrapper::WrapFuture;
use crate::{scope, FutureObj, JoinHandle, Scope};

used_in_docs!(Scope, scope);

pub(crate) struct ScopeExecutor<'env> {
    executor: Executor<FutureObj<'env, ()>>,
    panic: Mutex<Option<Payload>>,
}

impl<'env> ScopeExecutor<'env> {
    pub fn new() -> Self {
        Self {
            executor: Executor::new(),
            panic: Mutex::new(None),
        }
    }

    pub fn unhandled_panic(&self) -> Option<Payload> {
        let mut panic = self.panic.lock().unwrap_or_else(|e| e.into_inner());
        panic.take()
    }

    pub fn set_unhandled_panic(&self, payload: Payload) {
        let mut panic = self.panic.lock().unwrap_or_else(|e| e.into_inner());
        panic.get_or_insert(payload);
    }

    pub fn spawn_direct(&self, future: FutureObj<'env, ()>) {
        self.executor.spawn_direct(future);
    }

    pub fn spawn(&self, future: FutureObj<'env, ()>) {
        self.executor.spawn(future);
    }

    pub fn clear(&self) {
        self.executor.clear()
    }

    /// Create a [`ScopeHandle`] for this `ScopeExecutor`.
    ///
    /// The returned [`ScopeHandle`] has an arbitrary lifetime because what it
    /// really refers to is the lifetime of the `ScopeExecutor` stored within.
    /// It is not possible to name this lifetime since it is ultimately
    /// determined by runtime behaviour.
    ///
    /// This function effectively creates another reference to the Arc without
    /// incrementing its reference count.
    ///
    /// # Safety
    /// The `'scope` lifetime of the reference returned by this function _must_
    /// end before any mutable reference is made to the `ScopeExecutor` value
    /// stored within the `Arc`. If not upheld then there may be concurrent
    /// mutable and immutable borrows on the executor.
    ///
    /// Generally the way to do this is to call [`clear`] before the
    /// `ScopeExecutor` is dropped.
    ///
    /// [`clear`]: ScopeExecutor::clear
    pub unsafe fn handle<'scope>(&self) -> ScopeHandle<'scope, 'env>
    where
        'env: 'scope,
    {
        // SAFETY: The caller guarantees that the resulting reference will not outlive
        //         the scope itself.
        let this = unsafe { &*(self as *const Self) };

        ScopeHandle::new(this)
    }

    pub fn poll(&self, cx: &mut Context<'_>) -> Poll<()> {
        self.executor.poll(cx)
    }
}

/// A handle to an [`Scope`] that allows spawning scoped tasks on it.
///
/// This is provided to the closure passed to the [`scope!`] macro.
///
/// See the [crate] docs for details.
#[derive(Copy, Clone)]
pub struct ScopeHandle<'scope, 'env: 'scope> {
    executor: &'scope ScopeExecutor<'scope>,

    /// 'env represents the external environment.
    ///
    /// It needs to outlive 'scope but as long as that remains true we can
    /// shorten it arbitrarily.
    ///
    /// This makes it covariant.
    ///
    /// ScopeExecutor is invariant over 'env. On its own this is fine, but since
    /// a scope handle is used within a self-referential borrow rust's
    /// variance inference can't infer the right variance.
    _env: Covariant<'env>,

    /// 'scope needs to be invariant.
    ///
    /// Allowing 'scope to be shortened would allow borrowing values within the
    /// scope. Allowing the code below is clearly invalid.
    /// ```text
    /// scope!(|scope| {
    ///     let a = ();
    ///
    ///     scope.spawn(async { println!("{}", a) });
    /// })
    /// ```
    ///
    /// Allowing 'scope to be extended would allow extending the lifetime beyond
    /// the end of the scope, which would allow you to return the ScopeHandle.
    /// Since the handle holds a reference to internal state of the scope this
    /// is also clearly invalid.
    ///
    /// So 'scope needs to be invariant.
    _scope: Invariant<'scope>,
}

impl<'scope, 'env: 'scope> ScopeHandle<'scope, 'env> {
    fn new(executor: &'scope ScopeExecutor<'env>) -> Self {
        // # Safety
        // This closes the loop and makes the `ScopeExecutor` lifetime accurate.
        // Note that `'env` is outlives `'scope`.
        //
        // Externally, the `ScopeExecutor` has the `'env` lifetime since there is no
        // other available lifetime (besides `'static`, which is even less accurate).
        // However, 'scope is really self-referential since it contains references to
        // itself via `ScopeHandle` instances.
        //
        // The executor API ensures that no reference to data contained within the
        // executor is allowed to escape so this is safe.
        //
        // See the documentation on `_scope` and `_env` to see why the lifetimes are the
        // way they are.
        let executor: &'scope ScopeExecutor<'scope> =
            unsafe { &*(executor as *const ScopeExecutor<'env> as *const _) };

        Self {
            executor,
            _env: Covariant::new(),
            _scope: Invariant::new(),
        }
    }

    /// Spawn a new task within the scope, returning a [`JoinHandle`] to it.
    ///
    /// Unlike a non-scoped spawn, threads spawned with this function may borrow
    /// non-`'static` data from outside the scope. See the [`crate`] docs for
    /// details.
    ///
    /// The join handle can be awaited on to join the spawned task. If the
    /// spawned tasks panics then the output of awaiting the [`JoinHandle`] will
    /// be an [`Err`] containing the panic payload.
    ///
    /// If the join handle is dropped then the spawned task will be implicitly
    /// joined at the end of the scope. In that case, the scope will panic after
    /// all tasks have been joined. If [`Scope::cancel_remaining_tasks_on_exit`]
    /// has been set to `true`, then the scope will not join tasks but will
    /// still panic after all tasks are canceled.
    ///
    /// If the [`JoinHandle`] outlives the scope and is then dropped, then the
    /// panic will be lost.
    pub fn spawn<F>(&self, future: F) -> JoinHandle<'scope, 'env, F::Output>
    where
        F: Future + Send + 'scope,
        F::Output: Send + 'scope,
    {
        let (future, handle) = WrapFuture::new_send(future, *self);
        self.executor.spawn(future);
        handle
    }

    pub(crate) fn mark_unhandled_panic(&self, payload: Payload) {
        self.executor.set_unhandled_panic(payload);
    }
}
