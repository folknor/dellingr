//! Functions and types associated with converting source code into bytecode.

mod exp_desc;
mod lexer;
mod parser;
mod token;

use std::cell::Cell;

use super::Instr;
use super::Result;
use super::error;

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

#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Chunk {
    pub(super) code: Vec<Instr>,
    pub(super) number_literals: Vec<f64>,
    pub(super) string_literals: Vec<Vec<u8>>,
    pub(super) global_lookup_cache: Vec<GlobalLookupCacheSlot>,
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
        let mut cache_indices = vec![None; self.string_literals.len()];
        let mut cache_len = 0usize;

        for inst in &mut self.code {
            if inst.opcode() == Instr::OP_GET_GLOBAL {
                let string_idx = inst.a() as usize;
                let Some(cache_idx) = cache_indices.get_mut(string_idx) else {
                    continue;
                };
                let cache_idx = match *cache_idx {
                    Some(cache_idx) => cache_idx,
                    None => {
                        let next_idx =
                            u16::try_from(cache_len).expect("too many global lookup cache slots");
                        *cache_idx = Some(next_idx);
                        cache_len += 1;
                        next_idx
                    }
                };
                *inst = Instr::get_global_cached(inst.a(), cache_idx);
            }
        }

        self.global_lookup_cache = (0..cache_len)
            .map(|_| GlobalLookupCacheSlot::default())
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
mod tests {
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
}
