//! These marker types are confusing and seeing their implementations scattered
//! around the code is confusing. This module provides some centralized
//! definitions that can be used throughout the rest of the code.

use std::marker::PhantomData;

/// Mark a lifetime as being invariant.
#[derive(Copy, Clone, Debug, Default)]
pub struct Invariant<'a>(PhantomData<fn(&'a ()) -> &'a ()>);

impl<'a> Invariant<'a> {
    pub const fn new() -> Self {
        Self(PhantomData)
    }
}

#[derive(Copy, Clone, Debug, Default)]
pub struct Covariant<'a>(PhantomData<&'a ()>);

impl<'a> Covariant<'a> {
    pub const fn new() -> Self {
        Self(PhantomData)
    }
}
