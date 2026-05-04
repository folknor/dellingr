//! Support for global substitution with capture references.
//!
//! ```ignore
//! let mut m = lua_patterns2::LuaPattern::new("(%S+)%s*=%s*(%S+);%s*");
//! let res = m.gsub("a=2; b=3; c = 4;", "'%2':%1 ");
//! assert_eq!(res,"'2':a '3':b '4':c ");
//! ```

use super::{Captures, LuaPattern};
use std::string::{String, ToString};
use std::vec::Vec;

impl<'a> LuaPattern<'a> {
    /// Globally substitute all matches with a replacement string
    ///
    /// This string _may_ have capture references ("%0",..). Use "%%"
    /// to represent "%". Plain strings like "" work just fine ;)
    ///
    /// ```ignore
    /// let mut m = lua_patterns2::LuaPattern::new("(%S+)%s*=%s*(%S+);%s*");
    /// let res = m.gsub("a=2; b=3; c = 4;", "'%2':%1 ");
    /// assert_eq!(res,"'2':a '3':b '4':c ");
    /// ```
    pub fn gsub(&mut self, text: &str, repl: &str) -> String {
        let repl = generate_gsub_patterns(repl);
        let mut slice = text;
        let mut res = String::new();
        while self.matches(slice) {
            let all = self.range();
            res.push_str(&slice[0..all.start]);
            let captures = Captures {
                m: self,
                text: slice,
            };
            for r in &repl {
                match *r {
                    Subst::Text(ref s) => res.push_str(&s),
                    Subst::Capture(i) => res.push_str(captures.get(i)),
                }
            }
            slice = &slice[all.end..];
        }
        res.push_str(slice);
        res
    }
}

#[derive(Debug)]
pub enum Subst {
    Text(String),
    Capture(usize),
}

impl Subst {
    fn new_text(text: &str) -> Subst {
        Subst::Text(text.to_string())
    }
}

pub fn generate_gsub_patterns(repl: &str) -> Vec<Subst> {
    let mut m = LuaPattern::new("%%([%%%d])");
    let mut res = Vec::new();
    let mut slice = repl;
    while m.matches(slice) {
        let all = m.range();
        let before = &slice[0..all.start];
        if before != "" {
            res.push(Subst::new_text(before));
        }
        let capture = &slice[m.capture(1)];
        if capture == "%" {
            // escaped literal '%'
            res.push(Subst::new_text("%"));
        } else {
            // has to be a digit
            let index: usize = capture.parse().unwrap();
            res.push(Subst::Capture(index));
        }
        slice = &slice[all.end..];
    }
    res.push(Subst::new_text(slice));
    res
}

pub struct Substitute {
    repl: Vec<Subst>,
}

impl Substitute {
    pub fn new(repl: &str) -> Substitute {
        Substitute {
            repl: generate_gsub_patterns(repl),
        }
    }

    pub fn subst(&self, patt: &LuaPattern, text: &str) -> String {
        let mut res = String::new();
        let captures = patt.match_captures(text);
        for r in &self.repl {
            match *r {
                Subst::Text(ref s) => res.push_str(&s),
                Subst::Capture(i) => res.push_str(captures.get(i)),
            }
        }
        res
    }
}
