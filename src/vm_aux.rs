//! More functions on State.

use crate::LuaType;
use crate::Result;
use crate::State;
use crate::error::ArgError;
use crate::error::ErrorKind;
use crate::lua_std;

impl State {
    /// Verify that the host call has at least `arg_number` arguments on the
    /// stack. Returns an [`ArgError`] if not. Argument indices are 1-based;
    /// negative values count from the top of the stack.
    pub fn check_any(&mut self, arg_number: isize) -> Result<()> {
        assert!(arg_number != 0);
        if self.get_top() < arg_number.unsigned_abs() {
            let e = ArgError {
                arg_number,
                func_name: None,
                expected: None,
                received: None,
            };
            Err(self.error(ErrorKind::ArgError(e)))
        } else {
            Ok(())
        }
    }

    /// Verify that argument `arg_number` is of the given Lua type. Returns
    /// an [`ArgError`] if missing or of the wrong type. Argument indices are
    /// 1-based; negative values count from the top of the stack.
    pub fn check_type(&mut self, arg_number: isize, expected_type: LuaType) -> Result<()> {
        assert!(arg_number != 0);
        if self.get_top() < arg_number.unsigned_abs() {
            let e = ArgError {
                arg_number,
                func_name: None,
                expected: Some(expected_type),
                received: None,
            };
            return Err(self.error(ErrorKind::ArgError(e)));
        }
        let received_type = self.typ(arg_number);
        if received_type != expected_type {
            let e = ArgError {
                arg_number,
                func_name: None,
                expected: Some(expected_type),
                received: Some(received_type),
            };
            return Err(self.error(ErrorKind::ArgError(e)));
        }
        Ok(())
    }

    /// Opens all standard Lua libraries.
    #[hotpath::measure]
    pub fn open_libs(&mut self) {
        lua_std::open_libs(self);
        #[cfg(feature = "snapshot")]
        self.capture_env_tokens();
    }
}
