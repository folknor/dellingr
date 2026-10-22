use super::ExpDesc;
use super::Instr;
use super::Parser;
use super::Result;
use super::RetCount;
use super::SyntaxError;
use super::TailKind;
use super::TokenKind;
use crate::limits::tuning::{INLINE_TEMPLATE_FIELDS, TABLE_INLINE_CAPACITY};
use crate::limits::{MAX_TABLE_BATCH_SIZE, MAX_TABLE_TEMPLATE_FIELDS, MAX_TABLE_TEMPLATES};

type TableTemplateField = (u16, usize);
type ParsedTableEntry = (u8, Option<TailKind>, Option<TableTemplateField>);

#[derive(Debug)]
struct TableTemplateCandidate {
    pure_named: bool,
    inline: [TableTemplateField; INLINE_TEMPLATE_FIELDS],
    inline_len: u8,
    fields: Option<Vec<TableTemplateField>>,
}

impl TableTemplateCandidate {
    fn new() -> Self {
        Self {
            pure_named: true,
            inline: [(0, 0); INLINE_TEMPLATE_FIELDS],
            inline_len: 0,
            fields: None,
        }
    }

    fn push(&mut self, field: Option<TableTemplateField>) {
        if !self.pure_named {
            return;
        }

        let Some(field) = field else {
            self.pure_named = false;
            self.fields = None;
            return;
        };

        if let Some(fields) = self.fields.as_mut() {
            fields.push(field);
        } else if usize::from(self.inline_len) < INLINE_TEMPLATE_FIELDS {
            self.inline[usize::from(self.inline_len)] = field;
            self.inline_len += 1;
        } else {
            let mut fields = Vec::with_capacity(INLINE_TEMPLATE_FIELDS + 1);
            fields.extend_from_slice(&self.inline);
            fields.push(field);
            self.fields = Some(fields);
        }
    }

    fn fields_for_template(&self, field_count: u8) -> Option<&[TableTemplateField]> {
        if self.pure_named && usize::from(field_count) > INLINE_TEMPLATE_FIELDS {
            self.fields.as_deref()
        } else {
            None
        }
    }
}

impl Parser<'_> {
    /// Parses a table constructor.
    #[hotpath::measure]
    pub(super) fn parse_table(&mut self) -> Result<()> {
        let table_instr_idx = self.funcs.chunk.code.len();
        self.push(Instr::new_table());
        // Skip any leading separators (handles {;} and {,}). This is an
        // intentional divergence from Lua 5.2/5.4, which reject a leading or
        // lone separator; docs/compatibility.md documents it.
        while let TokenKind::Comma | TokenKind::Semi = self.input.peek_type()? {
            self.input.next()?;
        }
        if self.input.try_pop(TokenKind::RCurly)?.is_none() {
            // i is the number of array-style entries.
            let mut i = 0;
            let mut batch = 0u16;
            let mut field_count = 0u8;
            // Named and computed-key fields: the hash-capacity hint. The
            // positional part needs no hint, because each SET_LIST batch
            // reserves the array for itself.
            let mut keyed_count = 0u8;
            let mut template_candidate = TableTemplateCandidate::new();
            let last_tail = loop {
                if i == MAX_TABLE_BATCH_SIZE {
                    self.push(Instr::set_list_batch(i, batch));
                    i = 0;
                    batch = batch
                        .checked_add(1)
                        .ok_or_else(|| self.error_here(SyntaxError::TooManyTableFields))?;
                }
                let (new_i, tail, named_field) = self.parse_table_entry(i)?;
                if new_i == i {
                    keyed_count = keyed_count.saturating_add(1);
                }
                i = new_i;
                field_count = field_count.saturating_add(1);
                template_candidate.push(named_field);

                if !matches!(self.input.peek_type()?, TokenKind::Comma | TokenKind::Semi) {
                    break tail;
                }
                self.input.next()?;
                if self.input.check_type(TokenKind::RCurly)? {
                    break tail;
                }
            };
            self.expect(TokenKind::RCurly)?;

            if let Some(tail) = last_tail.as_ref() {
                self.open_tail(*tail, RetCount::All)?;
                self.funcs.chunk.code[table_instr_idx] = Instr::new_table_tracked(keyed_count);
                self.push(Instr::set_list_batch(0, batch));
            } else if usize::from(field_count) > INLINE_TEMPLATE_FIELDS
                && !template_candidate
                    .fields_for_template(field_count)
                    .is_some_and(|fields| self.try_use_table_template(table_instr_idx, fields))
                && usize::from(keyed_count) > TABLE_INLINE_CAPACITY
            {
                self.funcs.chunk.code[table_instr_idx] = Instr::new_table_presized(keyed_count);
            }

            if last_tail.is_none() && i > 0 {
                self.push(Instr::set_list_batch(i, batch));
            }
        }
        Ok(())
    }

    /// Parses a table entry.
    /// Returns (new_counter, final multi-value tail candidate, template field).
    #[hotpath::measure]
    fn parse_table_entry(&mut self, counter: u8) -> Result<ParsedTableEntry> {
        match self.input.peek_type()? {
            TokenKind::Identifier if self.input.peek2_type()? == TokenKind::Assign => {
                let index = self.expect_identifier_id()?;
                self.expect(TokenKind::Assign)?;
                self.parse_expr()?;
                let instr_idx = self.funcs.chunk.code.len();
                self.push(Instr::init_field(counter, index));
                Ok((counter, None, Some((index, instr_idx))))
            }
            TokenKind::LSquare => {
                self.input.next()?;
                self.parse_expr()?;
                self.expect(TokenKind::RSquare)?;
                self.expect(TokenKind::Assign)?;
                self.parse_expr()?;
                self.push(Instr::init_index(counter));
                Ok((counter, None, None))
            }
            _ => {
                let expr = self.parse_expr()?;
                let tail = match expr {
                    ExpDesc::MultiTail(tail) => Some(tail),
                    _ => None,
                };
                Ok((counter + 1, tail, None))
            }
        }
    }

    fn try_use_table_template(
        &mut self,
        table_instr_idx: usize,
        fields: &[TableTemplateField],
    ) -> bool {
        if fields.is_empty() || self.funcs.chunk.table_templates.len() >= MAX_TABLE_TEMPLATES {
            return false;
        }

        let mut template = Vec::with_capacity(fields.len());
        let mut field_indices = Vec::with_capacity(fields.len());
        for (key_id, _) in fields {
            if template.len() >= MAX_TABLE_TEMPLATE_FIELDS {
                return false;
            }
            if template.contains(key_id) {
                return false;
            }
            let entry_idx = match u8::try_from(template.len()) {
                Ok(entry_idx) => entry_idx,
                Err(_) => return false,
            };
            template.push(*key_id);
            field_indices.push(entry_idx);
        }

        // Templates are optional: an unencodable index keeps ordinary table
        // construction rather than rejecting the source.
        let Ok(template_idx) = u8::try_from(self.funcs.chunk.table_templates.len()) else {
            return false;
        };
        self.funcs.chunk.table_templates.push(template);
        self.funcs.chunk.code[table_instr_idx] = Instr::new_table_template(template_idx);
        for ((key_id, instr_idx), entry_idx) in fields.iter().zip(field_indices) {
            self.funcs.chunk.code[*instr_idx] = Instr::init_field_pinned(*key_id, entry_idx);
        }
        true
    }
}
