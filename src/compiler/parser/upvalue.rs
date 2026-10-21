use super::Parser;
use super::Result;
use super::SyntaxError;
use super::UpvalueDesc;
use super::find_last_local;
use crate::limits::MAX_UPVALUES;

impl<'a> Parser<'a> {
    /// Resolve a lexical name to a local or upvalue slot in the given frame.
    /// Locals win over existing upvalues; a miss recurses outward, then installs
    /// captures from outermost to innermost so descriptor indices keep their
    /// first-reference order.
    pub(super) fn resolve_in(
        &mut self,
        level: usize,
        name: &'a str,
    ) -> Result<Option<UpvalueDesc>> {
        let frame = &self.funcs.frames[level];
        if let Some(index) = find_last_local(&frame.locals, name) {
            return Ok(Some(UpvalueDesc::Local(index as u8)));
        }
        if let Some(index) = frame.upvalues.iter().position(|(n, _)| *n == name) {
            return Ok(Some(UpvalueDesc::Upvalue(index as u8)));
        }
        if level == 0 {
            return Ok(None);
        }
        let Some(desc) = self.resolve_in(level - 1, name)? else {
            return Ok(None);
        };
        let len = self.funcs.frames[level].upvalues.len();
        if len >= MAX_UPVALUES {
            return Err(self.error_here(SyntaxError::TooManyUpvalues));
        }
        self.funcs.frames[level].upvalues.push((name, desc));
        Ok(Some(UpvalueDesc::Upvalue(len as u8)))
    }
}
