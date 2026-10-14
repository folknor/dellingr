//! Callback attribution follows execution across independently loaded chunks.

use std::sync::{Arc, Mutex};

use dellingr::error::Error;
use dellingr::{ArgCount, HostCallbacks, RetCount, State};

#[derive(Default)]
struct Events {
    prints: Vec<(Option<String>, u32, String)>,
    errors: Vec<Option<String>>,
}

struct Recorder(Arc<Mutex<Events>>);

impl HostCallbacks for Recorder {
    fn on_print(&mut self, source: Option<&str>, line: u32, message: &str) {
        self.0.lock().expect("event lock is healthy").prints.push((
            source.map(str::to_owned),
            line,
            message.to_owned(),
        ));
    }

    fn on_error(&mut self, source: Option<&str>, _error: &Error) {
        self.0
            .lock()
            .expect("event lock is healthy")
            .errors
            .push(source.map(str::to_owned));
    }
}

fn load_and_run(state: &mut State, source: &str, name: &str) {
    state
        .load_string_named(source, Some(name.into()))
        .expect("chunk compiles");
    state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect("setup succeeds");
}

#[test]
fn nested_chunks_restore_source_and_attribute_error_to_running_frame() {
    let events = Arc::new(Mutex::new(Events::default()));
    let mut state = State::with_callbacks(Box::new(Recorder(Arc::clone(&events))));
    load_and_run(
        &mut state,
        "function a()\n print('a')\n b()\n print('again')\n error('failure')\nend",
        "A",
    );
    load_and_run(&mut state, "function b()\n print('b')\nend", "B");
    assert!(
        state
            .load_string_named("local =", Some("rejected".into()))
            .is_err()
    );
    assert_eq!(state.current_source(), None);
    state.get_global("a").expect("a is installed");
    let error = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("a fails");
    assert_eq!(error.stack_trace[0].source.as_deref(), Some("A"));
    assert_eq!(error.stack_trace[0].line, 5);
    assert_eq!(state.current_source(), None);
    let events = events.lock().expect("event lock is healthy");
    assert_eq!(
        events.prints,
        vec![
            (Some("A".into()), 2, "a".into()),
            (Some("B".into()), 2, "b".into()),
            (Some("A".into()), 4, "again".into()),
        ]
    );
    assert_eq!(events.errors, vec![Some("A".into())]);
}
