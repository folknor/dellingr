//! Functions and types associated with converting source code into bytecode.

mod exp_desc;
mod lexer;
mod parser;
mod token;

use std::cell::Cell;

use super::Instr;
use super::Result;
use super::error;
use super::vm::{ObjectPtr, Val};

/// Describes where an upvalue comes from when creating a closure.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum UpvalueDesc {
    /// Capture a local variable from the immediately enclosing function.
    Local(u8),
    /// Capture an upvalue from the immediately enclosing function.
    Upvalue(u8),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct GlobalLookupCacheEntry {
    pub(super) globals_version: u64,
    pub(super) index: usize,
}

#[derive(Debug, Default)]
pub(super) struct GlobalLookupCacheSlot {
    entry: Cell<Option<GlobalLookupCacheEntry>>,
}

impl GlobalLookupCacheSlot {
    pub(super) fn get(&self) -> Option<GlobalLookupCacheEntry> {
        self.entry.get()
    }

    pub(super) fn set(&self, entry: GlobalLookupCacheEntry) {
        self.entry.set(Some(entry));
    }
}

impl Clone for GlobalLookupCacheSlot {
    fn clone(&self) -> Self {
        // Runtime lookup caches are State-specific, so cloned chunks start cold.
        Self::default()
    }
}

impl PartialEq for GlobalLookupCacheSlot {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct FieldLookupCacheEntry {
    pub(super) table: ObjectPtr,
    pub(super) table_version: u64,
    pub(super) index: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct MethodLookupCacheEntry {
    pub(super) receiver_metatable: ObjectPtr,
    pub(super) index_key: Val,
    pub(super) index_field_index: usize,
    pub(super) index_handler: Val,
    pub(super) method_table_version: u64,
    pub(super) method_index: Option<usize>,
}

#[derive(Debug, Default)]
pub(super) struct FieldLookupCacheSlot {
    field_entry: Cell<Option<FieldLookupCacheEntry>>,
    method_entry: Cell<Option<MethodLookupCacheEntry>>,
}

impl FieldLookupCacheSlot {
    pub(super) fn get_field(&self) -> Option<FieldLookupCacheEntry> {
        self.field_entry.get()
    }

    pub(super) fn set_field(&self, entry: FieldLookupCacheEntry) {
        self.field_entry.set(Some(entry));
    }

    pub(super) fn get_method(&self) -> Option<MethodLookupCacheEntry> {
        self.method_entry.get()
    }

    pub(super) fn set_method(&self, entry: MethodLookupCacheEntry) {
        self.method_entry.set(Some(entry));
    }
}

impl Clone for FieldLookupCacheSlot {
    fn clone(&self) -> Self {
        // Runtime lookup caches are State-specific, so cloned chunks start cold.
        Self::default()
    }
}

impl PartialEq for FieldLookupCacheSlot {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

#[derive(Debug, Default)]
pub(super) struct SetFieldLookupCacheSlot {
    entry: Cell<Option<FieldLookupCacheEntry>>,
}

impl SetFieldLookupCacheSlot {
    pub(super) fn get(&self) -> Option<FieldLookupCacheEntry> {
        self.entry.get()
    }

    pub(super) fn set(&self, entry: FieldLookupCacheEntry) {
        self.entry.set(Some(entry));
    }
}

impl Clone for SetFieldLookupCacheSlot {
    fn clone(&self) -> Self {
        // Runtime lookup caches are State-specific, so cloned chunks start cold.
        Self::default()
    }
}

impl PartialEq for SetFieldLookupCacheSlot {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Chunk {
    pub(super) code: Vec<Instr>,
    pub(super) number_literals: Vec<f64>,
    pub(super) string_literals: Vec<Vec<u8>>,
    pub(super) global_lookup_cache: Vec<GlobalLookupCacheSlot>,
    pub(super) field_lookup_cache: Vec<FieldLookupCacheSlot>,
    pub(super) set_field_lookup_cache: Vec<SetFieldLookupCacheSlot>,
    pub(super) num_params: u8,
    pub(super) num_locals: u8,
    pub(super) nested: Vec<Chunk>,
    /// Describes the upvalues this function captures.
    pub(super) upvalues: Vec<UpvalueDesc>,
    /// Whether this function accepts varargs (...).
    pub(super) is_vararg: bool,
    /// Optional function name (for debugging/analysis).
    pub(super) name: Option<String>,
    /// Source name (file path or chunk identifier like "[string]").
    pub(super) source: Option<String>,
    /// Maps instruction index to source line number.
    /// line_info[i] is the line number for code[i].
    pub(super) line_info: Vec<u32>,
}

impl Chunk {
    fn initialize_runtime_caches(&mut self) {
        let mut global_cache_indices = vec![None; self.string_literals.len()];
        let mut global_cache_len = 0usize;
        let mut field_cache_len = 0usize;
        let mut set_field_cache_len = 0usize;

        for inst in &mut self.code {
            match inst.opcode() {
                Instr::OP_GET_GLOBAL => {
                    let string_idx = inst.a() as usize;
                    let Some(cache_idx) = global_cache_indices.get_mut(string_idx) else {
                        continue;
                    };
                    let cache_idx = match *cache_idx {
                        Some(cache_idx) => cache_idx,
                        None => {
                            let next_idx = u16::try_from(global_cache_len)
                                .expect("too many global lookup cache slots");
                            *cache_idx = Some(next_idx);
                            global_cache_len += 1;
                            next_idx
                        }
                    };
                    *inst = Instr::get_global_cached(inst.a(), cache_idx);
                }
                Instr::OP_GET_FIELD => {
                    let cache_idx =
                        u16::try_from(field_cache_len).expect("too many field lookup cache slots");
                    field_cache_len += 1;
                    *inst = Instr::get_field_cached(inst.a(), cache_idx);
                }
                Instr::OP_SET_FIELD => {
                    let cache_idx = u8::try_from(set_field_cache_len)
                        .expect("too many set-field lookup cache slots");
                    set_field_cache_len += 1;
                    *inst = Instr::set_field_cached(inst.a(), inst.b(), cache_idx);
                }
                _ => {}
            }
        }

        self.global_lookup_cache = (0..global_cache_len)
            .map(|_| GlobalLookupCacheSlot::default())
            .collect();
        self.field_lookup_cache = (0..field_cache_len)
            .map(|_| FieldLookupCacheSlot::default())
            .collect();
        self.set_field_lookup_cache = (0..set_field_cache_len)
            .map(|_| SetFieldLookupCacheSlot::default())
            .collect();
        for nested in &mut self.nested {
            nested.initialize_runtime_caches();
        }
    }
}

#[hotpath::measure]
pub(super) fn parse_str(source: impl AsRef<str>) -> Result<Chunk> {
    let mut chunk = parser::parse_str(source.as_ref())?;
    chunk.initialize_runtime_caches();
    Ok(chunk)
}

#[hotpath::measure]
pub(super) fn parse_str_named(
    source: impl AsRef<str>,
    source_name: Option<String>,
) -> Result<Chunk> {
    let mut chunk = parser::parse_str_named(source.as_ref(), source_name)?;
    chunk.initialize_runtime_caches();
    Ok(chunk)
}

#[cfg(test)]
mod runtime_cache_tests {
    use super::*;

    #[test]
    fn global_lookup_cache_tracks_distinct_get_global_names_only() {
        let chunk = parse_str(
            r#"
            local literal = "not a global"
            local t = { field = literal }
            foo = foo + foo
            bar = bar
            "#,
        )
        .unwrap();

        let get_globals: Vec<_> = chunk
            .code
            .iter()
            .filter(|inst| inst.opcode() == Instr::OP_GET_GLOBAL)
            .collect();

        assert_eq!(get_globals.len(), 3);
        assert_eq!(chunk.global_lookup_cache.len(), 2);
        assert!(chunk.string_literals.len() > chunk.global_lookup_cache.len());
        assert_eq!(get_globals[0].bx(), get_globals[1].bx());
        assert_ne!(get_globals[0].bx(), get_globals[2].bx());
    }

    #[test]
    fn field_lookup_cache_tracks_get_field_call_sites() {
        let chunk = parse_str(
            r#"
            local t = { x = 1, y = 2 }
            return t.x + t.x + t.y
            "#,
        )
        .unwrap();

        let get_fields: Vec<_> = chunk
            .code
            .iter()
            .filter(|inst| inst.opcode() == Instr::OP_GET_FIELD)
            .collect();

        assert_eq!(get_fields.len(), 3);
        assert_eq!(chunk.field_lookup_cache.len(), 3);
        assert_eq!(get_fields[0].bx(), 0);
        assert_eq!(get_fields[1].bx(), 1);
        assert_eq!(get_fields[2].bx(), 2);
    }

    #[test]
    fn set_field_lookup_cache_tracks_set_field_call_sites() {
        let chunk = parse_str(
            r#"
            local t = { x = 0, y = 0 }
            t.x = 1
            t.x = 2
            t.y = 3
            "#,
        )
        .unwrap();

        let set_fields: Vec<_> = chunk
            .code
            .iter()
            .filter(|inst| inst.opcode() == Instr::OP_SET_FIELD)
            .collect();

        assert_eq!(set_fields.len(), 3);
        assert_eq!(chunk.set_field_lookup_cache.len(), 3);
        assert_eq!(set_fields[0].c(), 0);
        assert_eq!(set_fields[1].c(), 1);
        assert_eq!(set_fields[2].c(), 2);
    }
}
