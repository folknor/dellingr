//! Persisted gmatch closure IDs and state keys. The runtime iterator in
//! `lua_std/string.rs` and the snapshot verifier in
//! `vm/save_state/verify.rs` both read these, so their schema literals cannot
//! drift.

pub(crate) const ITERATOR_ID: &str = "string.gmatch.iter";
pub(crate) const EMPTY_ITERATOR_ID: &str = "string.gmatch.empty_iter";

pub(crate) const SUBJECT_FIELD: &str = "s";
pub(crate) const PATTERN_FIELD: &str = "p";
// The empty and nonempty iterators share this persisted position key.
pub(crate) const POSITION_FIELD: &str = "pos";
pub(crate) const LENGTH_FIELD: &str = "len";
pub(crate) const LAST_MATCH_FIELD: &str = "lastmatch";
pub(crate) const COMPILE_CHARGED_FIELD: &str = "compile_charged";
pub(crate) const COMPILE_CHARGED_SLOT: f64 = 0.0;
