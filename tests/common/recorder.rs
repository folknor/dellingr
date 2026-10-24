//! Host callbacks that record what a State reports instead of printing it.

use std::sync::{Arc, Mutex, MutexGuard};

use dellingr::error::Error;
use dellingr::{HostCallbacks, State};

#[derive(Default)]
struct Recorded {
    prints: Vec<Vec<u8>>,
    errors: Vec<String>,
}

/// Records every `print` as its exact bytes and every `on_error`
/// notification as the error's rendered text. Clones share one record, so a
/// test keeps one and hands the other to the State.
#[derive(Clone, Default)]
pub(crate) struct Recorder(Arc<Mutex<Recorded>>);

impl Recorder {
    /// A standard State reporting to a new recorder.
    pub(crate) fn state() -> (State, Self) {
        let recorder = Self::default();
        (State::with_callbacks(Box::new(recorder.clone())), recorder)
    }

    fn recorded(&self) -> MutexGuard<'_, Recorded> {
        self.0.lock().expect("the recorder lock is not poisoned")
    }

    /// How many errors the State has reported.
    pub(crate) fn error_count(&self) -> usize {
        self.recorded().errors.len()
    }

    /// The rendered errors the State has reported, oldest first.
    pub(crate) fn errors(&self) -> Vec<String> {
        self.recorded().errors.clone()
    }

    /// The printed messages, oldest first.
    pub(crate) fn prints(&self) -> Vec<Vec<u8>> {
        self.recorded().prints.clone()
    }

    /// Forget everything recorded so far.
    pub(crate) fn clear(&self) {
        let mut recorded = self.recorded();
        recorded.prints.clear();
        recorded.errors.clear();
    }
}

impl HostCallbacks for Recorder {
    fn on_print(&mut self, _source: Option<&[u8]>, _line: u32, message: &[u8]) {
        self.recorded().prints.push(message.to_owned());
    }

    fn on_error(&mut self, error: &Error) {
        self.recorded().errors.push(error.to_string());
    }
}
