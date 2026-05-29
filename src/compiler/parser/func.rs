use std::sync::Arc;

use super::Instr;
use super::Parser;
use super::PlaceExp;
use super::Result;
use super::SyntaxError;
use super::TokenType;

impl<'a> Parser<'a> {
    /// Parses a function declaration, which is any statement that starts with
    /// the keyword `function`.
    pub(super) fn parse_fndecl(&mut self) -> Result<()> {
        self.input.next()?; // 'function' keyword
        let name = self.expect_identifier()?;
        match self.input.peek_type()? {
            TokenType::Dot => self.parse_fndecl_table(name),
            TokenType::Colon => self.parse_fndecl_method(name),
            _ => self.parse_fndecl_basic(name),
        }
    }

    /// Parses a basic function declaration, which just assigns the function to
    /// a local, upvalue, or global variable.
    fn parse_fndecl_basic(&mut self, name: &'a str) -> Result<()> {
        let place_exp = self.parse_prefix_identifier(name)?;
        let instr = match place_exp {
            PlaceExp::Local(i) => Instr::set_local(i),
            PlaceExp::Upvalue(i) => Instr::set_upvalue(i),
            PlaceExp::Global(i) => Instr::set_global(i),
            PlaceExp::Builtin(b) => Instr::set_builtin(b),
            _ => unreachable!("place expression was not a local, upvalue, or global variable"),
        };
        self.parse_fndef_named(Some(name.to_string()))?;
        self.push(instr);
        Ok(())
    }

    fn parse_fndecl_table(&mut self, table_name: &'a str) -> Result<()> {
        // Push the table onto the stack.
        let table_instr = match self.parse_prefix_identifier(table_name)? {
            PlaceExp::Local(i) => Instr::get_local(i),
            PlaceExp::Upvalue(i) => Instr::get_upvalue(i),
            PlaceExp::Global(i) => Instr::get_global(i),
            PlaceExp::Builtin(b) => Instr::get_builtin(b),
            _ => unreachable!("place expression was not a local, upvalue, or global variable"),
        };
        self.push(table_instr);

        // Parse all the fields, building the full name (e.g., "foo.bar.baz").
        let mut full_name = table_name.to_string();
        self.expect(TokenType::Dot)?;
        let mut last_field = self.expect_identifier()?;
        full_name.push('.');
        full_name.push_str(last_field);
        let mut last_field_id = self.find_or_add_string(last_field)?;

        while self.input.try_pop(TokenType::Dot)?.is_some() {
            self.push(Instr::get_field(last_field_id));
            last_field = self.expect_identifier()?;
            full_name.push('.');
            full_name.push_str(last_field);
            last_field_id = self.find_or_add_string(last_field)?;
        }

        if self.input.try_pop(TokenType::Colon)?.is_some() {
            self.push(Instr::get_field(last_field_id));
            let method_name = self.expect_identifier()?;
            full_name.push(':');
            full_name.push_str(method_name);
            let method_name_id = self.find_or_add_string(method_name)?;

            self.parse_fndef_method(Some(full_name))?;
            self.push(Instr::set_field(0, method_name_id));
        } else {
            self.parse_fndef_named(Some(full_name))?;
            self.push(Instr::set_field(0, last_field_id));
        }
        Ok(())
    }

    /// Parses a method declaration: `function table:method()`
    /// This is sugar for `table.method = function(self, ...)`
    fn parse_fndecl_method(&mut self, table_name: &'a str) -> Result<()> {
        // Push the table onto the stack.
        let table_instr = match self.parse_prefix_identifier(table_name)? {
            PlaceExp::Local(i) => Instr::get_local(i),
            PlaceExp::Upvalue(i) => Instr::get_upvalue(i),
            PlaceExp::Global(i) => Instr::get_global(i),
            PlaceExp::Builtin(b) => Instr::get_builtin(b),
            _ => unreachable!("place expression was not a local, upvalue, or global variable"),
        };
        self.push(table_instr);

        // Consume the colon and get the method name.
        self.expect(TokenType::Colon)?;
        let method_name = self.expect_identifier()?;
        let full_name = format!("{table_name}:{method_name}");
        let method_name_id = self.find_or_add_string(method_name)?;

        // Parse the function params and body with implicit self.
        self.parse_fndef_method(Some(full_name))?;
        self.push(Instr::set_field(0, method_name_id));
        Ok(())
    }

    /// Parses the parameters in a function definition.
    /// Returns (params, is_vararg).
    fn parse_params(&mut self) -> Result<(Vec<&'a str>, bool)> {
        let lparen_tok = self.input.next()?;
        match lparen_tok.typ {
            TokenType::LParen | TokenType::LParenLineStart => (),
            _ => return Err(self.err_unexpected(lparen_tok, TokenType::LParen)),
        }
        let mut args = Vec::new();
        let mut is_vararg = false;

        if self.input.try_pop(TokenType::RParen)?.is_some() {
            return Ok((args, is_vararg));
        }

        // Check for vararg-only function: function(...)
        if self.input.try_pop(TokenType::DotDotDot)?.is_some() {
            is_vararg = true;
            self.expect(TokenType::RParen)?;
            return Ok((args, is_vararg));
        }

        args.push(self.expect_identifier()?);
        while self.input.try_pop(TokenType::Comma)?.is_some() {
            // Check for vararg after comma: function(a, b, ...)
            if self.input.try_pop(TokenType::DotDotDot)?.is_some() {
                is_vararg = true;
                break;
            }
            args.push(self.expect_identifier()?);
        }
        self.expect(TokenType::RParen)?;
        Ok((args, is_vararg))
    }

    /// Parses the parameters and body of a function definition.
    pub(super) fn parse_fndef(&mut self) -> Result<()> {
        self.parse_fndef_named(None)
    }

    /// Parses the parameters and body of a function definition with an optional name.
    pub(super) fn parse_fndef_named(&mut self, name: Option<String>) -> Result<()> {
        let (params, is_vararg) = self.parse_params()?;
        if self.chunk.nested.len() >= u8::MAX as usize {
            return Err(self.error(SyntaxError::TooManyNestedFunctions));
        }

        self.nest_level += 1;
        let mut new_chunk = self.parse_chunk(&params, is_vararg)?;
        new_chunk.name = name;
        self.level_down();

        self.chunk.nested.push(Arc::new(new_chunk));
        self.push(Instr::closure(self.chunk.nested.len() as u8 - 1));
        self.expect(TokenType::End)?;
        Ok(())
    }

    /// Parses a method definition with implicit `self` parameter.
    /// Used for `function table:method()` syntax.
    fn parse_fndef_method(&mut self, name: Option<String>) -> Result<()> {
        let (mut params, is_vararg) = self.parse_params()?;
        // Prepend "self" to the parameter list
        params.insert(0, "self");
        if self.chunk.nested.len() >= u8::MAX as usize {
            return Err(self.error(SyntaxError::TooManyNestedFunctions));
        }

        self.nest_level += 1;
        let mut new_chunk = self.parse_chunk(&params, is_vararg)?;
        new_chunk.name = name;
        self.level_down();

        self.chunk.nested.push(Arc::new(new_chunk));
        self.push(Instr::closure(self.chunk.nested.len() as u8 - 1));
        self.expect(TokenType::End)?;
        Ok(())
    }
}
