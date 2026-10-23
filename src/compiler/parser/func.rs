use std::sync::Arc;

use super::super::ast::{Expr, ExprKind, Place, Stmt, Suffix, Suffixed};
use super::Parser;
use super::Result;
use super::SyntaxError;
use super::TokenKind;
use super::Variable;
use super::expr::variable_read;
use crate::limits::{MAX_CHUNK_NESTING, MAX_NESTED};

impl<'a> Parser<'a> {
    /// Parses a function declaration, which is any statement that starts with
    /// the keyword `function`.
    pub(super) fn parse_fndecl(&mut self) -> Result<Stmt> {
        let line = self.input.next()?.line; // 'function' keyword
        let name = self.expect_identifier()?;
        match self.input.peek_type()? {
            TokenKind::Dot => self.parse_fndecl_table(name, line),
            TokenKind::Colon => self.parse_fndecl_method(name, line),
            _ => self.parse_fndecl_basic(name, line),
        }
    }

    /// Parses a basic function declaration, which just assigns the function to
    /// a local, upvalue, or global variable.
    fn parse_fndecl_basic(&mut self, name: &'a str, line: u32) -> Result<Stmt> {
        let target = match self.parse_prefix_identifier(name)? {
            Variable::Local { reg, .. } => Place::Local(reg),
            Variable::Upval(index) => Place::Upval(index),
            Variable::Global(name) => Place::Global(name),
            Variable::Builtin(builtin) => Place::Builtin(builtin),
        };
        let (nested, _) = self.parse_fndef(Some(name.to_string()), false)?;
        Ok(Stmt::Function {
            target,
            nested,
            line,
        })
    }

    /// The table a dotted or method declaration stores into, read where its
    /// name was resolved.
    fn declaration_table(&mut self, table_name: &'a str) -> Result<Expr> {
        let table = self.parse_prefix_identifier(table_name)?;
        Ok(variable_read(table, self.input.last_line()))
    }

    fn parse_fndecl_table(&mut self, table_name: &'a str, line: u32) -> Result<Stmt> {
        let base = self.declaration_table(table_name)?;
        let mut suffixes = Vec::new();

        // Parse all the fields, building the full name (e.g., "foo.bar.baz").
        let mut full_name = table_name.to_string();
        self.expect(TokenKind::Dot)?;
        let mut last_field = self.expect_identifier()?;
        full_name.push('.');
        full_name.push_str(last_field);
        let mut last_field_id = self.find_or_add_string(last_field)?;

        while self.input.try_pop(TokenKind::Dot)?.is_some() {
            suffixes.push(Suffix::Field {
                name: last_field_id,
                line: self.input.last_line(),
            });
            last_field = self.expect_identifier()?;
            full_name.push('.');
            full_name.push_str(last_field);
            last_field_id = self.find_or_add_string(last_field)?;
        }

        let (name, method) = if self.input.try_pop(TokenKind::Colon)?.is_some() {
            suffixes.push(Suffix::Field {
                name: last_field_id,
                line: self.input.last_line(),
            });
            let method_name = self.expect_identifier()?;
            full_name.push(':');
            full_name.push_str(method_name);
            (self.find_or_add_string(method_name)?, true)
        } else {
            (last_field_id, false)
        };
        let object = if suffixes.is_empty() {
            base
        } else {
            Expr::new(ExprKind::Suffixed(Box::new(Suffixed { base, suffixes })))
        };
        let (nested, _) = self.parse_fndef(Some(full_name), method)?;
        Ok(Stmt::Function {
            target: Place::Field { object, name },
            nested,
            line,
        })
    }

    /// Parses a method declaration: `function table:method()`
    /// This is sugar for `table.method = function(self, ...)`
    fn parse_fndecl_method(&mut self, table_name: &'a str, line: u32) -> Result<Stmt> {
        let object = self.declaration_table(table_name)?;

        // Consume the colon and get the method name.
        self.expect(TokenKind::Colon)?;
        let method_name = self.expect_identifier()?;
        let full_name = format!("{table_name}:{method_name}");
        let name = self.find_or_add_string(method_name)?;

        // Parse the function params and body with implicit self.
        let (nested, _) = self.parse_fndef(Some(full_name), true)?;
        Ok(Stmt::Function {
            target: Place::Field { object, name },
            nested,
            line,
        })
    }

    /// Parses the parameters in a function definition.
    /// Returns (params, is_vararg).
    fn parse_params(&mut self) -> Result<(Vec<&'a str>, bool)> {
        let lparen_tok = self.input.next()?;
        match lparen_tok.typ {
            TokenKind::LParen => (),
            _ => return Err(self.err_unexpected(lparen_tok, TokenKind::LParen)),
        }
        let mut args = Vec::new();
        let mut is_vararg = false;

        if self.input.try_pop(TokenKind::RParen)?.is_some() {
            return Ok((args, is_vararg));
        }

        // Check for vararg-only function: function(...)
        if self.input.try_pop(TokenKind::DotDotDot)?.is_some() {
            is_vararg = true;
            self.expect(TokenKind::RParen)?;
            return Ok((args, is_vararg));
        }

        args.push(self.expect_identifier()?);
        while self.input.try_pop(TokenKind::Comma)?.is_some() {
            // Check for vararg after comma: function(a, b, ...)
            if self.input.try_pop(TokenKind::DotDotDot)?.is_some() {
                is_vararg = true;
                break;
            }
            args.push(self.expect_identifier()?);
        }
        self.expect(TokenKind::RParen)?;
        Ok((args, is_vararg))
    }

    /// Parses the parameters and body of a function definition with an
    /// optional diagnostic name. A `method` (`function t:m()`) gets the
    /// implicit leading `self` parameter. Returns the new function's
    /// nested-chunk index and the stamp of its closure creation.
    pub(super) fn parse_fndef(&mut self, name: Option<String>, method: bool) -> Result<(u8, u32)> {
        let defined_line = self.input.peek()?.line;
        let (mut params, is_vararg) = self.parse_params()?;
        if method {
            params.insert(0, "self");
        }
        if self.funcs.chunk.nested.len() >= MAX_NESTED {
            return Err(self.error_here(SyntaxError::TooManyNestedFunctions));
        }
        if self.funcs.frames.len() >= MAX_CHUNK_NESTING {
            return Err(self.error_here(SyntaxError::TooManySyntaxLevels));
        }

        let mut new_chunk = self.parse_chunk(&params, is_vararg)?;
        new_chunk.name = name.map(Arc::from);
        new_chunk.defined_line = defined_line;

        let index = self.bounded_u8(
            self.funcs.chunk.nested.len(),
            MAX_NESTED - 1,
            SyntaxError::TooManyNestedFunctions,
        )?;
        self.funcs.chunk.nested.push(Arc::new(new_chunk));
        let line = self.input.last_line();
        self.expect(TokenKind::End)?;
        Ok((index, line))
    }
}
