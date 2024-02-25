//! An arc with two values whose lifetimes are managed independently.
//!
//! This module has two main types:
//! - [`Full<H, T>`][Full] contains one value of type `H` and type `T`.
//! - [`Partial<H>`][Partial] can be obtained via [`Full::downgrade`] and shares
//!   the same memory but only refers to the value of type `H`.
//!
//! This allows having a [`Full`] that contains `T` with a restrictive lifetime
//! while still sharing [`Partial`] instances that may outlive that lifetime.
//! Other than that, both [`Full`] and [`Partial`] effectively behave like an
//! [`Arc`]. The only difference is that the `T` is dropped when the last
//! [`Full`] is dropped, even if there are [`Partial`] instances still live.
//!
//! [`Arc`]: std::sync::Arc

use std::cell::UnsafeCell;
use std::mem::ManuallyDrop;
use std::ops::Deref;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicUsize, Ordering};

struct HeaderData<H> {
    refcnt: AtomicUsize,
    datacnt: AtomicUsize,
    dealloc: unsafe fn(*mut HeaderData<H>),

    value: H,
}

// Ensure that casting *const FullData<H, T> to *const HeaderData<H> is valid.
#[repr(C)]
struct FullData<H, T> {
    header: HeaderData<H>,
    value: UnsafeCell<ManuallyDrop<T>>,
}

unsafe fn dealloc<H, T>(data: *mut HeaderData<H>) {
    // Note that the lifetime of T may be invalid here but that's OK because it is
    // within a ManuallyDrop and thus not touched in any way.
    let full = data.cast::<FullData<H, T>>();

    // SAFETY: The caller guarantees that data is valid and really points to a
    //         FullData<H, T> instance.
    let _ = unsafe { Box::from_raw(full) };
}

pub(crate) struct Full<H, T>(NonNull<FullData<H, T>>);

impl<H, T> Full<H, T> {
    pub fn new(header: H, value: T) -> Self {
        let ptr = Box::new(FullData {
            header: HeaderData {
                refcnt: AtomicUsize::new(1),
                datacnt: AtomicUsize::new(1),
                dealloc: dealloc::<H, T>,
                value: header,
            },
            value: UnsafeCell::new(ManuallyDrop::new(value)),
        });

        Self(NonNull::from(Box::leak(ptr)))
    }

    pub fn value(this: &Self) -> &T {
        // SAFETY: value is only accessed mutably during drop.
        unsafe { &*this.get().value.get() }
    }

    pub fn downgrade(this: &Self) -> Partial<H> {
        this.get().header.refcnt.fetch_add(1, Ordering::Relaxed);
        Partial(this.0.cast::<HeaderData<H>>())
    }

    fn get(&self) -> &FullData<H, T> {
        // SAFETY: self.0 will always be valid for Full
        unsafe { self.0.as_ref() }
    }
}

impl<H, T> Deref for Full<H, T> {
    type Target = H;

    fn deref(&self) -> &Self::Target {
        &self.get().header.value
    }
}

impl<H, T> Clone for Full<H, T> {
    fn clone(&self) -> Self {
        let data = self.get();

        data.header.refcnt.fetch_add(1, Ordering::Relaxed);
        data.header.datacnt.fetch_add(1, Ordering::Relaxed);

        Self(self.0)
    }
}

impl<H, T> Drop for Full<H, T> {
    fn drop(&mut self) {
        let data = self.get();

        let datacnt = data.header.datacnt.fetch_sub(1, Ordering::Release);
        if datacnt == 1 {
            // We need to synchronize with drops in other threads so that modifications in
            // other threads are visible once we actually drop data.value.
            //
            // This syncronizes with the decrement of datacnt.
            std::sync::atomic::fence(Ordering::Acquire);

            // SAFETY: Since we just decremented datacnt to 0 we are the only handle left
            //         with access to data.value.
            let value = unsafe { &mut *data.value.get() };

            // SAFETY: We are the last reference with access to value. No other code drops
            //         it the data count will only go to 0 once so there will be no
            //         duplicate drops.
            unsafe { ManuallyDrop::drop(value) };
        }

        let refcnt = data.header.refcnt.fetch_sub(1, Ordering::Release);
        if refcnt == 1 {
            // We need to synchronize with drops in other threads so that modifications in
            // other threads are visible once we actually drop data.
            //
            // This syncronizes with the decrement of refcnt.
            std::sync::atomic::fence(Ordering::Acquire);

            // SAFETY: self.0 is a valid pointer up to this instant. This drop only occurs
            //         when refcnt hits 0 and so it can only happen once.
            let _ = unsafe { Box::from_raw(self.0.as_ptr()) };
        }
    }
}

// SAFETY: Cloning Full allows for accessing both H and T from multiple threads
//         if Self is Send so both H and T need to be Send + Sync in order for
//         us to implement Send.
unsafe impl<H, T> Send for Full<H, T>
where
    H: Send + Sync,
    T: Send + Sync,
{
}

// SAFETY: See the comment on Send, the same constraints apply here.
unsafe impl<H, T> Sync for Full<H, T>
where
    H: Send + Sync,
    T: Send + Sync,
{
}

pub(crate) struct Partial<H>(NonNull<HeaderData<H>>);

impl<H> Partial<H> {
    fn get(&self) -> &HeaderData<H> {
        // SAFETY: self.0 will always be valid for Partial
        unsafe { self.0.as_ref() }
    }
}

impl<H> Deref for Partial<H> {
    type Target = H;

    fn deref(&self) -> &Self::Target {
        &self.get().value
    }
}

impl<H> Clone for Partial<H> {
    fn clone(&self) -> Self {
        self.get().refcnt.fetch_add(1, Ordering::Relaxed);
        Self(self.0)
    }
}

impl<H> Drop for Partial<H> {
    fn drop(&mut self) {
        let data = self.get();

        let refcnt = data.refcnt.fetch_sub(1, Ordering::Release);
        if refcnt == 1 {
            // We need to synchronize with drops in other threads so that modifications in
            // other threads are visible once we actually drop data.
            //
            // This syncronizes with the decrement of refcnt.
            std::sync::atomic::fence(Ordering::Acquire);

            let dealloc = data.dealloc;
            // SAFETY:
            // - self.0 is a valid pointer. It is only deallocated when refcnt hits 0 and
            //   that only happened right now.
            // - self.0 really points to the FullData<H, T> that dealloc expects. This is
            //   guaranteed by the Full constructor.
            unsafe { dealloc(self.0.as_ptr()) };
        }
    }
}

// SAFETY: Partial can be cloned to allow accessing H from multiple threads if
//         it is Send so H must be Send + Sync.
//
// Note: the T stored in the backing allocation need not be Send + Sync for
//       Partial to be Send. Partial will never perform any operation on the T
//       (not even drop) so if it is not Send then that causes no issue.
unsafe impl<H: Send + Sync> Send for Partial<H> {}

// SAFETY: See the comment on the Send impl.
unsafe impl<H: Send + Sync> Sync for Partial<H> {}
