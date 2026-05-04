use super::Result;
use super::State;
use super::object::{Closure, GcHeap, ObjectPtr, StringPtr};

use std::fmt;
use std::hash::{Hash, Hasher};

pub type RustFunc = fn(&mut State) -> Result<u8>;

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

    /// Get this value as a string, if it is one.
    /// Requires heap access since strings are stored in the GC heap.
    pub(super) fn as_string<'a>(&self, heap: &'a GcHeap) -> Option<&'a str> {
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
            RustFn(func) => format!("<function: {func:p}>"),
            Obj(o) => format!("{o}"),
            Str(s) => heap.get_string(s).to_string(),
        }
    }
}

impl fmt::Debug for Val {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Nil => write!(f, "nil"),
            Bool(b) => b.fmt(f),
            Num(n) => n.fmt(f),
            RustFn(func) => write!(f, "<function: {func:p}>"),
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
            RustFn(func) => write!(f, "<function: {func:p}>"),
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
            RustFn(func) => {
                let f: *const RustFunc = func;
                f.hash(hasher);
            }
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
            (RustFn(a), RustFn(b)) => {
                let x: *const RustFunc = a;
                let y: *const RustFunc = b;
                x == y
            }
            (Obj(a), Obj(b)) => a == b,
            // String pointer equality works because strings are interned
            (Str(a), Str(b)) => a == b,
            _ => false,
        }
    }
}

// Markable impl for Val is in object.rs

#[derive(Debug, Eq, PartialEq)]
pub enum LuaType {
    Nil,
    Boolean,
    Number,
    String,
    Table,
    Function,
}

impl LuaType {
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
