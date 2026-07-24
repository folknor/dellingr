//! Lua's standard library

mod basic;
mod math;
mod string;
mod string_format;
mod table;

pub(crate) use basic::{base_ipairs_iter, base_next, open_base};

use crate::State;

pub(crate) fn open_libs(state: &mut State) {
    state.reserve_stdlib_capacity();
    open_base(state);
    math::open_math(state);
    string::open_string(state);
    table::open_table(state);
}
