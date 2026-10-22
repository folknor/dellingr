//! Callback attribution follows execution across independently loaded chunks.

use std::sync::{Arc, Mutex};

use dellingr::error::Error;
use dellingr::{ArgCount, HostCallbacks, Program, RetCount, State};

#[derive(Default)]
struct Events {
    prints: Vec<(Option<Vec<u8>>, u32, Vec<u8>)>,
    errors: Vec<Option<Vec<u8>>>,
}

struct Recorder(Arc<Mutex<Events>>);

impl HostCallbacks for Recorder {
    fn on_print(&mut self, source: Option<&[u8]>, line: u32, message: &[u8]) {
        self.0.lock().expect("event lock is healthy").prints.push((
            source.map(<[u8]>::to_owned),
            line,
            message.to_owned(),
        ));
    }

    fn on_error(&mut self, error: &Error) {
        self.0
            .lock()
            .expect("event lock is healthy")
            .errors
            .push(error.source_name().map(<[u8]>::to_owned));
    }
}

fn load_and_run(state: &mut State, source: &str, name: &[u8]) {
    let program = Program::compile(source, Some(name)).expect("chunk compiles");
    state.load(&program).expect("chunk loads");
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
        b"A",
    );
    load_and_run(&mut state, "function b()\n print('b')\nend", b"B");
    assert!(Program::compile("local =", Some(b"rejected".as_slice())).is_err());
    assert_eq!(state.current_source(), None);
    state.get_global("a").expect("a is installed");
    let error = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("a fails");
    assert_eq!(
        error.stack_trace()[0].source.as_deref(),
        Some(b"A".as_slice())
    );
    assert_eq!(error.stack_trace()[0].line, 5);
    assert_eq!(state.current_source(), None);
    let events = events.lock().expect("event lock is healthy");
    assert_eq!(
        events.prints,
        vec![
            (Some(b"A".to_vec()), 2, b"a".to_vec()),
            (Some(b"B".to_vec()), 2, b"b".to_vec()),
            (Some(b"A".to_vec()), 4, b"again".to_vec()),
        ]
    );
    assert_eq!(events.errors, vec![Some(b"A".to_vec())]);
}

#[test]
fn trace_frames_share_the_chunk_source_and_outlive_the_state() {
    let mut state = State::new();
    load_and_run(
        &mut state,
        "function inner()\n error('boom')\nend\nfunction outer()\n inner()\nend",
        b"shared",
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
    assert_eq!(&**current, b"shared");
    assert!(Arc::ptr_eq(current, caller));
    assert_eq!(error.to_string(), before);
}

#[test]
fn trace_frames_share_the_function_name() {
    let mut state = State::new();
    load_and_run(
        &mut state,
        "function rec(n)\n if n == 0 then error('boom') end\n rec(n - 1)\nend",
        b"recursive",
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

#[test]
fn chunk_names_and_printed_bytes_reach_the_host_exactly() {
    let events = Arc::new(Mutex::new(Events::default()));
    let mut state = State::with_callbacks(Box::new(Recorder(Arc::clone(&events))));
    // A non-UTF-8 chunk name and a non-UTF-8 source byte in a literal.
    let name: &[u8] = b"scripts/\xffbad.lua";
    let program = Program::compile(b"print('a\xfe', 1)\nerror('x')", Some(name))
        .expect("non-UTF-8 source compiles");
    assert_eq!(program.source_name(), Some(name));
    state.load(&program).expect("chunk loads");
    let error = state
        .call(ArgCount::Fixed(0), RetCount::Fixed(0))
        .expect_err("chunk raises");
    assert_eq!(error.source_name(), Some(name));
    assert_eq!(error.stack_trace()[0].source.as_deref(), Some(name));
    assert!(
        error
            .to_string()
            .starts_with("scripts/\u{FFFD}bad.lua:2: x"),
        "{error}"
    );
    let events = events.lock().expect("event lock is healthy");
    assert_eq!(
        events.prints,
        vec![(Some(name.to_vec()), 1, b"a\xfe\t1".to_vec())]
    );
    assert_eq!(events.errors, vec![Some(name.to_vec())]);
}
