//! Lua's standard library

mod basic;
mod math;
mod string;
mod table;

pub(crate) use basic::open_base;

use crate::State;

pub(crate) fn open_libs(state: &mut State) {
    open_base(state);
    math::open_math(state);
    string::open_string(state);
    table::open_table(state);
}
