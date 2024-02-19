/// Make a T: Send value Sync + Send by disallowing all immutable access to the
/// value.
pub(crate) struct SyncWrapper<T>(T);

// SAFETY: SyncWrapper disallows immutable access to T so moving an immutable
//         reference to SyncWrapper across threads is safe.
#[allow(unsafe_code)]
unsafe impl<T: Sync> Send for SyncWrapper<T> {}

impl<T> SyncWrapper<T> {
    pub fn new(value: T) -> Self {
        Self(value)
    }

    pub fn into_inner(self) -> T {
        self.0
    }
}
