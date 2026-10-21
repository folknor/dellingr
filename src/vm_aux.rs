//! More functions on State.

use crate::LuaType;
use crate::Result;
use crate::State;
use crate::error::ArgError;
use crate::error::ErrorKind;
use crate::error::LibraryError;
use crate::lua_std;

impl State {
    /// Construct a standard-library runtime error while retaining its typed
    /// reason for hosts that need to inspect it.
    pub(crate) fn library_error(&self, detail: LibraryError) -> crate::error::Error {
        self.error(ErrorKind::RuntimeError(detail.to_string()))
            .with_library_error(detail)
    }

    /// Verify that the host call has at least `arg_number` arguments on the
    /// stack. Returns an [`ArgError`] if not, or [`ErrorKind::InvalidStackIndex`]
    /// if the argument index is zero. Argument indices are 1-based; negative
    /// values count from the top of the stack.
    #[deny(clippy::panic, clippy::manual_assert)]
    pub fn check_any(&mut self, arg_number: isize) -> Result<()> {
        if arg_number == 0 {
            return Err(self.error(ErrorKind::InvalidStackIndex { index: arg_number }));
        }
        if self.get_top() < arg_number.unsigned_abs() {
            let func_name = self.active_rust_function_name();
            let e = ArgError {
                arg_number,
                func_name,
                detail: crate::error::ArgDetail::TypeMismatch,
                expected: None,
                received: None,
            };
            Err(self.error(ErrorKind::ArgError(e)))
        } else {
            Ok(())
        }
    }

    /// Verify that argument `arg_number` is of the given Lua type. Returns
    /// an [`ArgError`] if missing or of the wrong type, or
    /// [`ErrorKind::InvalidStackIndex`] if the argument index is zero.
    /// Argument indices are 1-based; negative values count from the top of
    /// the stack.
    #[deny(clippy::panic, clippy::manual_assert)]
    pub fn check_type(&mut self, arg_number: isize, expected_type: LuaType) -> Result<()> {
        if arg_number == 0 {
            return Err(self.error(ErrorKind::InvalidStackIndex { index: arg_number }));
        }
        if self.get_top() < arg_number.unsigned_abs() {
            let func_name = self.active_rust_function_name();
            let e = ArgError::type_mismatch(arg_number, func_name.as_deref(), expected_type, None);
            return Err(self.error(ErrorKind::ArgError(e)));
        }
        let received_type = self.try_typ(arg_number)?;
        if received_type != expected_type {
            let func_name = self.active_rust_function_name();
            let e = ArgError::type_mismatch(
                arg_number,
                func_name.as_deref(),
                expected_type,
                Some(received_type),
            );
            return Err(self.error(ErrorKind::ArgError(e)));
        }
        Ok(())
    }

    /// Verify that an argument belongs to a list of accepted Lua types.
    ///
    /// The first type owns the rendered expectation, preserving a library
    /// function's established message while the check accepts additional
    /// types. Missing arguments have Lua's visible `nil` type here, as in the
    /// string library's string-or-number checks.
    pub(crate) fn check_type_in(
        &mut self,
        arg_number: isize,
        accepted_types: &[LuaType],
        function_name: &str,
    ) -> Result<()> {
        if arg_number == 0 {
            return Err(self.error(ErrorKind::InvalidStackIndex { index: arg_number }));
        }
        let expected_type = *accepted_types
            .first()
            .expect("a type check requires at least one accepted type");
        let received_type = self.typ(arg_number);
        if accepted_types.contains(&received_type) {
            return Ok(());
        }
        let error = ArgError::type_mismatch(
            arg_number,
            Some(function_name),
            expected_type,
            Some(received_type),
        );
        Err(self.error(ErrorKind::ArgError(error)))
    }

    /// Returns whether argument `arg_number` is missing or explicitly nil.
    pub(crate) fn is_none_or_nil(&self, arg_number: isize) -> bool {
        // This private helper receives crate-owned argument positions. A zero
        // index here indicates an internal caller bug.
        assert!(arg_number != 0);
        self.get_top() < arg_number.unsigned_abs() || self.typ(arg_number) == LuaType::Nil
    }

    /// Verifies an optional argument's type.
    ///
    /// Missing and nil arguments are absent and return `false`. All other
    /// values are checked by [`Self::check_type`] so their errors retain the
    /// usual argument number and type details.
    pub(crate) fn check_optional_type(
        &mut self,
        arg_number: isize,
        expected_type: LuaType,
    ) -> Result<bool> {
        if self.is_none_or_nil(arg_number) {
            return Ok(false);
        }
        self.check_type(arg_number, expected_type)?;
        Ok(true)
    }

    /// Opens all standard Lua libraries.
    #[hotpath::measure]
    pub fn open_libs(&mut self) -> Result<()> {
        lua_std::open_libs(self)?;
        #[cfg(feature = "snapshot")]
        self.capture_env_tokens();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn optional_type_treats_missing_and_nil_as_absent() {
        let mut state = State::new();
        assert!(
            !state
                .check_optional_type(1, LuaType::Number)
                .expect("missing optional argument must be accepted")
        );

        state
            .push_nil()
            .expect("an empty stack has room for one value");
        assert!(
            !state
                .check_optional_type(1, LuaType::Number)
                .expect("nil optional argument must be accepted")
        );
    }

    #[test]
    fn optional_type_preserves_non_nil_arg_error() {
        let mut state = State::new();
        state
            .push_string("wrong type")
            .expect("short test string fits");

        let optional = state
            .check_optional_type(1, LuaType::Number)
            .expect_err("wrong optional argument type must fail");
        let required = state
            .check_type(1, LuaType::Number)
            .expect_err("wrong required argument type must fail");

        let (ErrorKind::ArgError(optional), ErrorKind::ArgError(required)) =
            (&optional.kind, &required.kind)
        else {
            panic!("both checks must produce argument errors");
        };
        assert_eq!(optional.arg_number, required.arg_number);
        assert_eq!(optional.expected, required.expected);
        assert_eq!(optional.received, required.received);
        assert_eq!(optional.func_name, required.func_name);
    }
}
