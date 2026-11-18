use super::Result;
use super::RustFnId;
use super::State;
#[cfg(test)]
use super::object::Closure;
use super::object::{GcHeap, ObjectPtr, StringPtr};

use std::fmt;
use std::hash::{Hash, Hasher};

/// Signature for a host-provided Rust function callable from Lua.
///
/// The function reads its arguments from the [`State`]'s stack (1-based
/// indexing for host calls), pushes any return values, and returns the
/// number of return values (or an error).
///
/// The returned count `n` selects the top `n` values of the function's frame
/// (arguments count, so returning an argument without pushing it is valid);
/// anything below them is discarded. A count larger than the number of values
/// in the frame is a host bug and fails the call with
/// [`ErrorKind::ResultCountExceedsStack`](crate::error::ErrorKind::ResultCountExceedsStack);
/// the missing results are not padded with nil.
//
// A typed callback context (decoded scalar arguments, borrowed views, a
// result writer) in place of `&mut State` and the stack protocol was weighed
// and refused: it changes every host and library signature, and the payoff is
// in cheap library bodies, which are reached by identity instead (the leaves
// in `vm/leaf.rs`, which skip the whole callback ABI). Leaving every
// callback's arguments in place is refused at the remove in `begin_call`.
pub type RustFunc = fn(&mut State) -> Result<u8>;

const RUST_FN_DISPLAY: &str = "<function>";

#[derive(Clone, Copy, Default)]
pub(crate) enum Val {
    #[default]
    Nil,
    Bool(bool),
    Num(f64),
    Str(StringPtr),
    /// An entry of the State's Rust function table (see `vm/rust_fn.rs`).
    RustFn(RustFnId),
    Obj(ObjectPtr),
}
use Val::*;

impl Val {
    /// Get this value as a Lua function (closure), if it is one.
    /// Requires heap access since the function data is stored in the GC heap.
    /// Test-only: runtime paths borrow through `GcHeap::as_lua_function_ref`.
    #[cfg(test)]
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
    /// Use heap.as_table_ref() or heap.as_lua_function_ref() to inspect its type.
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

    /// Render a primitive value. Pointer-like values have no heap-only
    /// representation because their deterministic identity needs State.
    pub(super) fn to_bytes_with_heap(self, heap: &GcHeap) -> Option<Vec<u8>> {
        match self {
            Nil => Some(b"nil".to_vec()),
            Bool(false) => Some(b"false".to_vec()),
            Bool(true) => Some(b"true".to_vec()),
            Num(n) => Some(crate::number_text::number_to_text(n).as_bytes().to_vec()),
            RustFn(_) | Obj(_) => None,
            Str(s) => Some(heap.get_string(s).to_vec()),
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
            // `ObjectPtr` is intentionally opaque in Debug: its slotmap key
            // is process-local and can vary between replay runs.
            Obj(o) => o.fmt(f),
            // Val has no heap reference here, so render a stable marker rather
            // than exposing StringPtr's process-local slotmap key.
            Str(_) => f.write_str("<string>"),
        }
    }
}

// No `Display for Val`. Rendering an object needs the heap for its type;
// objects and Rust functions need the State for their deterministic identity.
// A heap-free impl could only leak the slotmap key's `Debug` form - which is
// exactly the leak this avoids.
// User-visible conversion goes through `State::to_string`/
// `bytes_with_default_string_coercion`.

// `Val` equality keeps Lua's NaN behavior, so it is deliberately not `Eq` and
// has no `Hash`. Table storage keys use the validated `TableKey` wrapper and
// the `%p` identity index uses `PointerIdKey`.
impl PartialEq for Val {
    fn eq(&self, other: &Val) -> bool {
        match (self, other) {
            (Nil, Nil) => true,
            (Bool(a), Bool(b)) => a == b,
            (Num(a), Num(b)) => a == b,
            (RustFn(a), RustFn(b)) => a == b,
            (Obj(a), Obj(b)) => a == b,
            // String pointer equality works because strings are interned
            (Str(a), Str(b)) => a == b,
            _ => false,
        }
    }
}

/// A pointer-like runtime value used as the key of the `%p` identity index.
/// Primitive values cannot be represented, which keeps invalid table keys such
/// as NaN out of this map by construction.
#[derive(Clone, Copy)]
pub(crate) enum PointerIdKey {
    /// An interned string identity.
    String(StringPtr),
    /// A garbage-collected object identity.
    Object(ObjectPtr),
    /// A Rust function table entry.
    RustFn(RustFnId),
}

impl PointerIdKey {
    /// Convert a pointer-like value to an index key.
    pub(crate) fn from_val(value: Val) -> Option<Self> {
        match value {
            Val::Str(ptr) => Some(Self::String(ptr)),
            Val::Obj(ptr) => Some(Self::Object(ptr)),
            Val::RustFn(func) => Some(Self::RustFn(func)),
            Val::Nil | Val::Bool(_) | Val::Num(_) => None,
        }
    }

    /// Recreate the runtime value represented by this identity key.
    pub(crate) fn as_val(self) -> Val {
        match self {
            Self::String(ptr) => Val::Str(ptr),
            Self::Object(ptr) => Val::Obj(ptr),
            Self::RustFn(func) => Val::RustFn(func),
        }
    }
}

impl PartialEq for PointerIdKey {
    fn eq(&self, other: &Self) -> bool {
        match (*self, *other) {
            (Self::String(a), Self::String(b)) => a == b,
            (Self::Object(a), Self::Object(b)) => a == b,
            (Self::RustFn(a), Self::RustFn(b)) => a == b,
            _ => false,
        }
    }
}

impl Eq for PointerIdKey {}

impl Hash for PointerIdKey {
    fn hash<H: Hasher>(&self, hasher: &mut H) {
        match self {
            Self::String(ptr) => ptr.hash(hasher),
            Self::Object(ptr) => ptr.hash(hasher),
            // The entry index follows the order entries were created, so it
            // may only feed hash-bucket layout, like the slotmap keys above.
            Self::RustFn(function) => function.hash(hasher),
        }
    }
}

// Markable impl for Val is in object.rs

/// The runtime type of a Lua value, as exposed to host code via [`State`].
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
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

    #[test]
    fn rust_function_copies_compare_equal_and_hash_equal() {
        let first = Val::RustFn(RustFnId::catalog(0));
        let second = first;
        assert_eq!(first, second);

        let first = PointerIdKey::from_val(first).expect("Rust functions have pointer identity");
        let second = PointerIdKey::from_val(second).expect("Rust functions have pointer identity");
        assert!(first == second);
        let mut first_hash = RecordingHasher::default();
        let mut second_hash = RecordingHasher::default();
        first.hash(&mut first_hash);
        second.hash(&mut second_hash);
        assert_eq!(first_hash.0, second_hash.0);
    }

    /// Identity is the table entry, not the code: two entries for one Rust
    /// function are two values, and one entry pushed twice is one value.
    #[test]
    fn rust_function_identity_is_the_entry_not_the_function() {
        let mut state = State::raw();
        // One name for both: a name is not identity either.
        let first = state.add_rust_fn("same", first_callback).unwrap();
        let second = state.add_rust_fn("same", first_callback).unwrap();
        state.push_rust_fn(first).unwrap();
        state.push_rust_fn(second).unwrap();
        state.push_rust_fn(first).unwrap();
        assert!(!state.raw_equal(1, 2));
        assert!(state.raw_equal(1, 3));
    }

    #[test]
    fn rust_function_debug_rendering_is_opaque() {
        let value = Val::RustFn(RustFnId::catalog(0));
        assert_eq!(format!("{value:?}"), RUST_FN_DISPLAY);
    }

    #[test]
    fn string_debug_rendering_does_not_expose_pointer_identity() {
        let mut state = State::new();
        let value = state
            .alloc_string("example")
            .expect("small test string fits the VM string limit");
        assert_eq!(format!("{value:?}"), "<string>");
    }
}
