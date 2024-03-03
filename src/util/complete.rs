#[derive(Default)]
pub(crate) struct RequirePending {
    #[cfg(debug_assertions)]
    ready: bool,
}

impl RequirePending {
    pub fn new() -> Self {
        Self::default()
    }

    #[cfg(debug_assertions)]
    pub fn is_ready(&self) -> bool {
        self.ready
    }

    #[cfg(not(debug_assertions))]
    pub fn is_ready(&self) -> bool {
        true
    }

    #[cfg(debug_assertions)]
    pub fn set_ready(&mut self) {
        self.ready = true;
    }

    #[cfg(not(debug_assertions))]
    pub fn set_ready(&mut self) {}
}
