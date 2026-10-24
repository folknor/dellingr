//! Suites about the save format and `State::load_state` themselves, compiled
//! only under the `snapshot` feature. A save-and-load case of another
//! subject's behaviour (a cache, a rendering, a limit) stays in that
//! subject's module under `cfg(feature = "snapshot")`, beside the cases it
//! extends.

mod ipairs_save_state;
mod open_libs_registry;
mod save_golden;
mod save_state;
mod save_state_builtin_alias;
mod save_state_error_paths;
mod save_state_identity;
