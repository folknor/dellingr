use super::super::ast::{Expr, ExprKind, TableAlloc, TableExpr, TableItem};
use super::Parser;
use super::Result;
use super::SyntaxError;
use super::TokenKind;
use crate::limits::tuning::{INLINE_TEMPLATE_FIELDS, TABLE_INLINE_CAPACITY};
use crate::limits::{MAX_TABLE_BATCH_SIZE, MAX_TABLE_TEMPLATE_FIELDS, MAX_TABLE_TEMPLATES};

/// A named field's key literal and the index of its constructor item.
type TableTemplateField = (u16, usize);

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

/// One parsed constructor entry.
enum Entry {
    Positional(Expr),
    Named { name: u16, value: Expr, line: u32 },
    Keyed { key: Expr, value: Expr, line: u32 },
}

impl Parser<'_> {
    /// Parses a table constructor after its opening brace, which was read on
    /// `open_line`.
    #[hotpath::measure]
    pub(super) fn parse_table(&mut self, open_line: u32) -> Result<Expr> {
        let mut table = TableExpr {
            alloc: TableAlloc::Plain,
            line: open_line,
            items: Vec::new(),
        };
        // Skip any leading separators (handles {;} and {,}). This is an
        // intentional divergence from Lua 5.2/5.4, which reject a leading or
        // lone separator; docs/compatibility.md documents it.
        while let TokenKind::Comma | TokenKind::Semi = self.input.peek_type()? {
            self.input.next()?;
        }
        if self.input.try_pop(TokenKind::RCurly)?.is_none() {
            // i is the number of pending array-style entries.
            let mut i = 0u8;
            let mut batch = 0u16;
            let mut field_count = 0u8;
            // Named and computed-key fields: the hash-capacity hint. The
            // positional part needs no hint, because each SET_LIST batch
            // reserves the array for itself.
            let mut keyed_count = 0u8;
            let mut template_candidate = TableTemplateCandidate::new();
            let last_positional_multi = loop {
                if i == MAX_TABLE_BATCH_SIZE {
                    table.items.push(TableItem::Flush {
                        count: Some(i),
                        batch,
                        line: self.input.last_line(),
                    });
                    i = 0;
                    batch = batch
                        .checked_add(1)
                        .ok_or_else(|| self.error_here(SyntaxError::TooManyTableFields))?;
                }
                let entry = self.parse_table_entry()?;
                field_count = field_count.saturating_add(1);
                let mut tail_multi = false;
                let item = match entry {
                    Entry::Positional(value) => {
                        i += 1;
                        tail_multi = value.is_multi();
                        template_candidate.push(None);
                        TableItem::Positional(value)
                    }
                    Entry::Named { name, value, line } => {
                        keyed_count = keyed_count.saturating_add(1);
                        template_candidate.push(Some((name, table.items.len())));
                        TableItem::Named {
                            name,
                            value,
                            line,
                            pinned: None,
                        }
                    }
                    Entry::Keyed { key, value, line } => {
                        keyed_count = keyed_count.saturating_add(1);
                        template_candidate.push(None);
                        TableItem::Keyed { key, value, line }
                    }
                };
                table.items.push(item);

                if !matches!(self.input.peek_type()?, TokenKind::Comma | TokenKind::Semi) {
                    break tail_multi;
                }
                self.input.next()?;
                if self.input.check_type(TokenKind::RCurly)? {
                    break tail_multi;
                }
            };
            self.expect(TokenKind::RCurly)?;

            if last_positional_multi {
                // The open final batch takes every pending value up to the
                // top, the expandable tail's values included.
                table.alloc = TableAlloc::Presized(keyed_count);
                table.items.push(TableItem::Flush {
                    count: None,
                    batch,
                    line: self.input.last_line(),
                });
            } else {
                if usize::from(field_count) > INLINE_TEMPLATE_FIELDS
                    && !template_candidate
                        .fields_for_template(field_count)
                        .is_some_and(|fields| self.try_use_table_template(&mut table, fields))
                    && usize::from(keyed_count) > TABLE_INLINE_CAPACITY
                {
                    table.alloc = TableAlloc::Presized(keyed_count);
                }
                if i > 0 {
                    table.items.push(TableItem::Flush {
                        count: Some(i),
                        batch,
                        line: self.input.last_line(),
                    });
                }
            }
        }
        Ok(Expr::new(ExprKind::Table(Box::new(table))))
    }

    /// Parses a table entry.
    #[hotpath::measure]
    fn parse_table_entry(&mut self) -> Result<Entry> {
        match self.input.peek_type()? {
            TokenKind::Identifier if self.input.peek2_type()? == TokenKind::Assign => {
                let name = self.expect_identifier_id()?;
                self.expect(TokenKind::Assign)?;
                let value = self.parse_expr()?;
                Ok(Entry::Named {
                    name,
                    value,
                    line: self.input.last_line(),
                })
            }
            TokenKind::LSquare => {
                self.input.next()?;
                let key = self.parse_expr()?;
                self.expect(TokenKind::RSquare)?;
                self.expect(TokenKind::Assign)?;
                let value = self.parse_expr()?;
                Ok(Entry::Keyed {
                    key,
                    value,
                    line: self.input.last_line(),
                })
            }
            _ => Ok(Entry::Positional(self.parse_expr()?)),
        }
    }

    fn try_use_table_template(
        &mut self,
        table: &mut TableExpr,
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
        table.alloc = TableAlloc::Template(template_idx);
        for ((_, item_idx), entry_idx) in fields.iter().zip(field_indices) {
            if let Some(TableItem::Named { pinned, .. }) = table.items.get_mut(*item_idx) {
                *pinned = Some(entry_idx);
            }
        }
        true
    }
}
