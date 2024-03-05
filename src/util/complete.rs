#[derive(Default)]
pub(crate) struct RequirePending {
    #[cfg(debug_assertions)]
    ready: bool,
}

impl RequirePending {
    pub fn new() -> Self {
        Self::default()
    }
}

#[cfg(debug_assertions)]
impl RequirePending {
    pub fn is_ready(&self) -> bool {
        self.ready
    }

    pub fn set_ready(&mut self) {
        self.ready = true;
    }
}

#[cfg(not(debug_assertions))]
impl RequirePending {
    pub fn is_ready(&self) -> bool {
        true
    }

    pub fn set_ready(&mut self) {}
}
