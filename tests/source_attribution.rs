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

    fn on_error(&mut self, error: &Error) {
        self.0
            .lock()
            .expect("event lock is healthy")
            .errors
            .push(error.source_name().map(str::to_owned));
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
    assert_eq!(error.stack_trace()[0].source.as_deref(), Some("A"));
    assert_eq!(error.stack_trace()[0].line, 5);
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

#[test]
fn trace_frames_share_the_chunk_source_and_outlive_the_state() {
    let mut state = State::new();
    load_and_run(
        &mut state,
        "function inner()\n error('boom')\nend\nfunction outer()\n inner()\nend",
        "shared",
    );
    state.get_global("outer").expect("outer is installed");
    let error = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("outer fails");
    let before = error.to_string();
    drop(state);
    let trace = error.stack_trace();
    // The current frame (built from the executing frame) and the caller frame
    // (built from the call stack) both hand out the chunk's own allocation.
    let current = trace[0]
        .source
        .as_ref()
        .expect("current frame has a source");
    let caller = trace[1].source.as_ref().expect("caller frame has a source");
    assert_eq!(&**current, "shared");
    assert!(Arc::ptr_eq(current, caller));
    assert_eq!(error.to_string(), before);
}

#[test]
fn trace_frames_share_the_function_name() {
    let mut state = State::new();
    load_and_run(
        &mut state,
        "function rec(n)\n if n == 0 then error('boom') end\n rec(n - 1)\nend",
        "recursive",
    );
    state.get_global("rec").expect("rec is installed");
    state.push_number(2.0).expect("argument fits");
    let error = state
        .call(ArgCount::Fixed(1), RetCount::Fixed(0))
        .expect_err("rec fails at the bottom");
    drop(state);
    let trace = error.stack_trace();
    // Every frame of the recursion names the same chunk, from both the
    // executing frame and the call stack, through one allocation.
    let current = trace[0]
        .function_name
        .as_ref()
        .expect("current frame is named");
    assert_eq!(&**current, "rec");
    for frame in &trace[1..3] {
        let name = frame.function_name.as_ref().expect("caller is named");
        assert!(Arc::ptr_eq(current, name));
    }
}
