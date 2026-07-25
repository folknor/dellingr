use super::ArgCount;
use super::ExpDesc;
use super::Instr;
use super::Parser;
use super::PrefixExp;
use super::Result;
use super::RetCount;
use super::SyntaxError;
use super::TokenType;

type TableTemplateField = (u16, usize);
type ParsedTableEntry = (u8, Option<TableTail>, Option<TableTemplateField>);

#[derive(Debug)]
enum TableTail {
    Call { instr_idx: usize },
    Vararg { instr_idx: usize },
}

#[derive(Debug)]
struct TableTemplateCandidate {
    pure_named: bool,
    inline: [TableTemplateField; 4],
    inline_len: u8,
    fields: Option<Vec<TableTemplateField>>,
}

impl TableTemplateCandidate {
    fn new() -> Self {
        Self {
            pure_named: true,
            inline: [(0, 0); 4],
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
        } else if (self.inline_len as usize) < self.inline.len() {
            self.inline[self.inline_len as usize] = field;
            self.inline_len += 1;
        } else {
            let mut fields = Vec::with_capacity(self.inline.len() + 1);
            fields.extend_from_slice(&self.inline);
            fields.push(field);
            self.fields = Some(fields);
        }
    }

    fn fields_for_template(&self, field_count: u8) -> Option<&[TableTemplateField]> {
        if self.pure_named && field_count > self.inline.len() as u8 {
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
        let table_instr_idx = self.chunk.code.len();
        self.push(Instr::new_table());
        // Skip any leading separators (handles {;} and {,}). This is an
        // INTENTIONAL divergence from Lua 5.2/5.4, which reject a leading or
        // lone separator; see examples/feature_test_extended.lua (tagged
        // `-- DIFF: semicolon_syntax`). Not a bug (lead L3).
        while let TokenType::Comma | TokenType::Semi = self.input.peek_type()? {
            self.input.next()?;
        }
        if self.input.try_pop(TokenType::RCurly)?.is_none() {
            // i is the number of array-style entries.
            let mut i = 0;
            let mut batch = 0u16;
            let mut field_count = 0u8;
            let mut template_candidate = TableTemplateCandidate::new();
            let last_tail = loop {
                if i == u8::MAX {
                    self.push(Instr::set_list_batch(i, batch));
                    i = 0;
                    batch = batch
                        .checked_add(1)
                        .ok_or_else(|| self.error(SyntaxError::TooManyTableFields))?;
                }
                let (new_i, tail, named_field) = self.parse_table_entry(i)?;
                i = new_i;
                field_count = field_count.saturating_add(1);
                template_candidate.push(named_field);

                if !matches!(self.input.peek_type()?, TokenType::Comma | TokenType::Semi) {
                    break tail;
                }
                self.input.next()?;
                if self.input.check_type(TokenType::RCurly)? {
                    break tail;
                }
            };
            self.expect(TokenType::RCurly)?;

            if let Some(tail) = last_tail.as_ref() {
                match tail {
                    TableTail::Call { instr_idx } => {
                        let instr = self.chunk.code[*instr_idx];
                        debug_assert_eq!(instr.opcode(), Instr::OP_CALL);
                        self.chunk.code[*instr_idx] =
                            Instr::call(ArgCount::from_u8(instr.a()), RetCount::All);
                    }
                    TableTail::Vararg { instr_idx } => {
                        debug_assert_eq!(self.chunk.code[*instr_idx].opcode(), Instr::OP_VARARG);
                        self.chunk.code[*instr_idx] = Instr::vararg(u8::MAX);
                    }
                }
                self.chunk.code[table_instr_idx] = Instr::new_table_tracked(field_count);
                self.push(Instr::set_list_batch(0, batch));
            } else if field_count > 4
                && !template_candidate
                    .fields_for_template(field_count)
                    .is_some_and(|fields| self.try_use_table_template(table_instr_idx, fields))
            {
                self.chunk.code[table_instr_idx] = Instr::new_table_presized(field_count);
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
            TokenType::Identifier if self.input.peek2_type()? == TokenType::Assign => {
                let index = self.expect_identifier_id()?;
                self.expect(TokenType::Assign)?;
                self.parse_expr()?;
                let instr_idx = self.chunk.code.len();
                self.push(Instr::init_field(counter, index));
                Ok((counter, None, Some((index, instr_idx))))
            }
            TokenType::LSquare => {
                self.input.next()?;
                self.parse_expr()?;
                self.expect(TokenType::RSquare)?;
                self.expect(TokenType::Assign)?;
                self.parse_expr()?;
                self.push(Instr::init_index(counter));
                Ok((counter, None, None))
            }
            _ => {
                let expr = self.parse_expr()?;
                let instr_idx = self.chunk.code.len() - 1;
                let tail = match expr {
                    ExpDesc::Prefix(PrefixExp::FunctionCall(_)) => {
                        Some(TableTail::Call { instr_idx })
                    }
                    ExpDesc::Vararg => Some(TableTail::Vararg { instr_idx }),
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
        if fields.is_empty() || self.chunk.table_templates.len() >= u8::MAX as usize {
            return false;
        }

        let mut template = Vec::with_capacity(fields.len());
        let mut field_indices = Vec::with_capacity(fields.len());
        for (key_id, _) in fields {
            if template.len() >= u8::MAX as usize {
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

        let template_idx = self.chunk.table_templates.len() as u8;
        self.chunk.table_templates.push(template);
        self.chunk.code[table_instr_idx] = Instr::new_table_template(template_idx);
        for ((key_id, instr_idx), entry_idx) in fields.iter().zip(field_indices) {
            self.chunk.code[*instr_idx] = Instr::init_field_pinned(*key_id, entry_idx);
        }
        true
    }
}
