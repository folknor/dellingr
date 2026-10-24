//! Debug-only observation of the lexical native-call inventory.

use std::cell::RefCell;
use std::collections::BTreeSet;

thread_local! {
    static ACTIVE: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
    static EDGES: RefCell<Option<BTreeSet<&'static str>>> = const { RefCell::new(None) };
}

#[doc(hidden)]
pub fn record(edge: &'static str) -> ActiveEdge {
    ACTIVE.with_borrow_mut(|active| active.push(edge));
    EDGES.with_borrow_mut(|edges| {
        if let Some(edges) = edges {
            edges.insert(edge);
        }
    });
    ActiveEdge
}

/// Keeps a marker live until its call expression returns or unwinds.
#[doc(hidden)]
pub struct ActiveEdge;

impl Drop for ActiveEdge {
    fn drop(&mut self) {
        ACTIVE.with_borrow_mut(|active| {
            active.pop();
        });
    }
}

/// Return the currently live call markers when an observer is active.
#[doc(hidden)]
pub fn active_path() -> Option<Vec<&'static str>> {
    EDGES.with_borrow(|edges| edges.as_ref().map(|_| ACTIVE.with_borrow(Clone::clone)))
}

/// Return whether a native-call observer is active on this thread.
#[doc(hidden)]
pub fn is_observing() -> bool {
    EDGES.with_borrow(Option::is_some)
}

/// Observe the distinct native-call edges on this thread during `observe`.
/// Nested observers are rejected; unwinding clears the observer too.
#[doc(hidden)]
pub fn with_native_call_trace<T>(observe: impl FnOnce() -> T) -> (T, Vec<&'static str>) {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            EDGES.with_borrow_mut(|edges| *edges = None);
        }
    }
    EDGES.with_borrow_mut(|edges| {
        assert!(edges.is_none(), "native-call observers cannot nest");
        *edges = Some(BTreeSet::new());
    });
    let reset = Reset;
    let result = observe();
    let edges = EDGES.with_borrow_mut(|edges| {
        edges
            .take()
            .expect("the observer owns this thread's edge set")
    });
    drop(reset);
    (result, edges.into_iter().collect())
}
