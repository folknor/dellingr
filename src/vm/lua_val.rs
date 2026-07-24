use super::Result;
use super::State;
use super::object::{Closure, GcHeap, ObjectPtr, StringPtr};

use std::borrow::Cow;
use std::fmt;
use std::hash::{Hash, Hasher};

/// Signature for a host-provided Rust function callable from Lua.
///
/// The function reads its arguments from the [`State`]'s stack (1-based
/// indexing for host calls), pushes any return values, and returns the
/// number of return values (or an error).
pub type RustFunc = fn(&mut State) -> Result<u8>;

const RUST_FN_DISPLAY: &str = "<function>";

#[derive(Clone, Copy, Default)]
pub(crate) enum Val {
    #[default]
    Nil,
    Bool(bool),
    Num(f64),
    Str(StringPtr),
    RustFn(RustFunc),
    Obj(ObjectPtr),
}
use Val::*;

impl Val {
    /// Get this value as a Lua function (closure), if it is one.
    /// Requires heap access since the function data is stored in the GC heap.
    pub(super) fn as_lua_function(&self, heap: &GcHeap) -> Option<Closure> {
        if let Obj(o) = self {
            heap.as_lua_function(*o)
        } else {
            None
        }
    }

    pub(super) fn as_num(&self) -> Option<f64> {
        match self {
            Num(f) => Some(*f),
            _ => None,
        }
    }

    /// Get this value as Lua string bytes, if it is a string.
    /// Requires heap access since strings are stored in the GC heap.
    pub(super) fn as_string<'a>(&self, heap: &'a GcHeap) -> Option<&'a [u8]> {
        if let Str(s) = self {
            Some(heap.get_string(*s))
        } else {
            None
        }
    }

    /// Get the StringPtr if this is a string value.
    pub(super) fn as_string_ptr(&self) -> Option<StringPtr> {
        if let Str(s) = self { Some(*s) } else { None }
    }

    /// Get the ObjectPtr if this is an object, without checking what kind.
    /// Use heap.as_table() or heap.as_lua_function() to check the actual type.
    pub(super) fn as_object_ptr(&self) -> Option<ObjectPtr> {
        if let Obj(o) = self { Some(*o) } else { None }
    }

    pub(super) fn truthy(&self) -> bool {
        !matches!(self, Nil | Bool(false))
    }

    /// Returns the value's type.
    /// Requires heap access to determine if an Object is a Table or Function.
    pub(super) fn typ(&self, heap: &GcHeap) -> LuaType {
        match self {
            Nil => LuaType::Nil,
            Bool(_) => LuaType::Boolean,
            Num(_) => LuaType::Number,
            RustFn(_) => LuaType::Function,
            Str(_) => LuaType::String,
            Obj(o) => o.typ(heap),
        }
    }

    /// Returns the value's type for non-object types.
    /// For objects, this is unsafe to call - use typ() with heap access instead.
    /// This is useful for error messages where we already know it's not an object.
    pub(super) fn typ_simple(&self) -> LuaType {
        match self {
            Nil => LuaType::Nil,
            Bool(_) => LuaType::Boolean,
            Num(_) => LuaType::Number,
            RustFn(_) => LuaType::Function,
            Str(_) => LuaType::String,
            Obj(_) => LuaType::Table, // Assume table for display purposes
        }
    }

    /// Convert this value to a string representation.
    /// Requires heap access to get string and object contents.
    pub(super) fn to_string_with_heap(self, heap: &GcHeap) -> String {
        match self {
            Nil => "nil".to_string(),
            Bool(b) => b.to_string(),
            Num(n) => n.to_string(),
            RustFn(_) => RUST_FN_DISPLAY.to_string(),
            Obj(o) => format!("{o}"),
            Str(s) => String::from_utf8_lossy(heap.get_string(s)).into_owned(),
        }
    }

    pub(super) fn to_bytes_with_heap(self, heap: &GcHeap) -> Cow<'_, [u8]> {
        match self {
            Nil => Cow::Borrowed(b"nil"),
            Bool(false) => Cow::Borrowed(b"false"),
            Bool(true) => Cow::Borrowed(b"true"),
            Num(n) => Cow::Owned(n.to_string().into_bytes()),
            RustFn(_) => Cow::Borrowed(b"<function>"),
            Obj(o) => Cow::Owned(format!("{o}").into_bytes()),
            Str(s) => Cow::Borrowed(heap.get_string(s)),
        }
    }
}

impl fmt::Debug for Val {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Nil => write!(f, "nil"),
            Bool(b) => b.fmt(f),
            Num(n) => n.fmt(f),
            RustFn(_) => f.write_str(RUST_FN_DISPLAY),
            Obj(o) => o.fmt(f),
            Str(s) => s.fmt(f),
        }
    }
}

impl fmt::Display for Val {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Nil => write!(f, "nil"),
            Bool(b) => b.fmt(f),
            Num(n) => n.fmt(f),
            Obj(o) => o.fmt(f),
            Str(s) => s.fmt(f),
            RustFn(_) => f.write_str(RUST_FN_DISPLAY),
        }
    }
}

/// This is very dangerous, since f64 doesn't implement Eq.
impl Eq for Val {}

impl Hash for Val {
    fn hash<H: Hasher>(&self, hasher: &mut H) {
        match self {
            Nil => (),
            Bool(b) => b.hash(hasher),
            Obj(o) => o.hash(hasher),
            Num(n) => {
                // NaN breaks HashMap invariants (compares unequal to itself but may hash same)
                // This must be a hard assert, not debug_assert, to prevent undefined behavior
                assert!(!n.is_nan(), "Cannot use NaN as table key");
                let mut bits = n.to_bits();
                if bits == 1 << 63 {
                    bits = 0;
                }
                bits.hash(hasher);
            }
            RustFn(func) => (*func as usize).hash(hasher),
            Str(s) => s.hash(hasher),
        }
    }
}

impl PartialEq for Val {
    fn eq(&self, other: &Val) -> bool {
        match (self, other) {
            (Nil, Nil) => true,
            (Bool(a), Bool(b)) => a == b,
            (Num(a), Num(b)) => a == b,
            (RustFn(a), RustFn(b)) => std::ptr::fn_addr_eq(*a, *b),
            (Obj(a), Obj(b)) => a == b,
            // String pointer equality works because strings are interned
            (Str(a), Str(b)) => a == b,
            _ => false,
        }
    }
}

// Markable impl for Val is in object.rs

/// The runtime type of a Lua value, as exposed to host code via [`State`].
#[derive(Debug, Eq, PartialEq)]
pub enum LuaType {
    /// `nil`
    Nil,
    /// `true` or `false`
    Boolean,
    /// IEEE-754 double-precision number
    Number,
    /// Interned UTF-8 string (or arbitrary byte string)
    String,
    /// Mutable Lua table (array + hash, with optional metatable)
    Table,
    /// Lua closure or host-provided [`RustFunc`]
    Function,
}

impl LuaType {
    /// Lowercase name used by Lua's `type()` builtin and in error messages.
    pub fn as_str(&self) -> &'static str {
        use LuaType::*;
        match self {
            Nil => "nil",
            Boolean => "boolean",
            Number => "number",
            String => "string",
            Table => "table",
            Function => "function",
        }
    }
}

impl fmt::Display for LuaType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_str().fmt(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct RecordingHasher(Vec<u8>);

    impl Hasher for RecordingHasher {
        fn finish(&self) -> u64 {
            0
        }

        fn write(&mut self, bytes: &[u8]) {
            self.0.extend_from_slice(bytes);
        }
    }

    fn first_callback(_state: &mut State) -> Result<u8> {
        Ok(0)
    }

    fn second_callback(state: &mut State) -> Result<u8> {
        state.push_number(1.0);
        Ok(1)
    }

    #[test]
    fn rust_function_copies_compare_equal_and_hash_equal() {
        let first = Val::RustFn(first_callback);
        let second = Val::RustFn(first_callback);
        assert_eq!(first, second);

        let mut first_hash = RecordingHasher::default();
        let mut second_hash = RecordingHasher::default();
        first.hash(&mut first_hash);
        second.hash(&mut second_hash);
        assert_eq!(first_hash.0, second_hash.0);
    }

    #[test]
    fn different_rust_functions_compare_unequal() {
        assert_ne!(Val::RustFn(first_callback), Val::RustFn(second_callback));
    }

    #[test]
    fn rust_function_rendering_is_consistent_and_allocation_free_for_bytes() {
        let value = Val::RustFn(first_callback);
        let state = State::empty();

        assert_eq!(format!("{value:?}"), RUST_FN_DISPLAY);
        assert_eq!(format!("{value}"), RUST_FN_DISPLAY);
        assert_eq!(value.to_string_with_heap(&state.heap), RUST_FN_DISPLAY);
        assert!(matches!(
            value.to_bytes_with_heap(&state.heap),
            Cow::Borrowed(b"<function>")
        ));
    }
}
