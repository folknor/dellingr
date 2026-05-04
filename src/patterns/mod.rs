//! This is a Rust binding to [Lua string patterns](https://www.lua.org/pil/20.2.html),
//! using the original code from Lua 5.2.
//!
//! Although not regular expressions (they lack alternation) they are a powerful
//! and lightweight way to process text. Please note that they are not
//! UTF-8-aware, and in fact can process arbitrary binary data.
//!
//! `LuaPattern` can be created from a string _or_ a byte slice, and has
//! methods which are similar to the original Lua API. Please see
//! [the README](https://github.com/stevedonovan/lua-patterns/blob/master/readme.md)
//! for more discussion.
//!
//! [LuaPattern](struct.LuaPattern.html) implements the public API.
//!
//! ## Examples
//!
//! ```ignore
//! extern crate lua_patterns2;
//! let mut m = lua_patterns2::LuaPattern::new("one");
//! let text = "hello one two";
//! assert!(m.matches(text));
//! let r = m.range();
//! assert_eq!(r.start, 6);
//! assert_eq!(r.end, 9);
//! ```
//!
//! Collecting captures from a match:
//!
//! ```ignore
//! extern crate lua_patterns2;
//! let text = "  hello one";
//! let mut m = lua_patterns2::LuaPattern::new("(%S+) one");
//!
//! // allocates a vector of captures
//! let v = m.captures(text);
//! assert_eq!(v, &["hello one","hello"]);
//! let mut v = Vec::new();
//! // writes captures into preallocated vector
//! if m.capture_into(text,&mut v) {
//!     assert_eq!(v, &["hello one","hello"]);
//! }
//! ```

#![allow(
    dead_code,
    elided_lifetimes_in_paths,
    explicit_outlives_requirements,
    single_use_lifetimes,
    trivial_numeric_casts,
    unexpected_cfgs,
    unreachable_pub,
    unused_imports,
    variant_size_differences,
    clippy::all,
    clippy::explicit_iter_loop,
    clippy::redundant_else,
    clippy::uninlined_format_args,
    clippy::unwrap_used
)]

use core::ops;
use std::string::{String, ToString};
use std::vec::Vec;

pub(crate) mod errors;
use self::errors::*;

pub(crate) mod builder;
pub(crate) mod subst;

mod luapat;
use self::luapat::*;

// If we run out of space in a fixed-capacity container, produce a partial result
#[cfg(feature = "heapless")]
type PartialResult<T> = Result<T, T>;

/// Represents a Lua string pattern and the results of a match
pub struct LuaPattern<'a> {
    patt: &'a [u8],
    matches: [LuaMatch; LUA_MAXCAPTURES],
    n_match: usize,
}

impl<'a> LuaPattern<'a> {
    /// Maybe create a new Lua pattern from a slice of bytes
    pub fn from_bytes_try(bytes: &'a [u8]) -> Result<LuaPattern<'a>, PatternError> {
        str_check(bytes)?;
        let matches = [LuaMatch { start: 0, end: 0 }; LUA_MAXCAPTURES];
        Ok(LuaPattern {
            patt: bytes,
            matches: matches,
            n_match: 0,
        })
    }

    /// Maybe create a new Lua pattern from a string
    pub fn new_try(patt: &'a str) -> Result<LuaPattern<'a>, PatternError> {
        LuaPattern::from_bytes_try(patt.as_bytes())
    }

    /// Create a new Lua pattern from a string, panicking if bad
    pub fn new(patt: &'a str) -> LuaPattern<'a> {
        LuaPattern::new_try(patt).expect("bad pattern")
    }

    /// Create a new Lua pattern from a slice of bytes, panicking if bad
    pub fn from_bytes(bytes: &'a [u8]) -> LuaPattern<'a> {
        LuaPattern::from_bytes_try(bytes).expect("bad pattern")
    }

    /// Match a slice of bytes with a pattern
    ///
    /// ```ignore
    /// let patt = &[0xFE,0xEE,b'+',0xED];
    /// let mut m = lua_patterns2::LuaPattern::from_bytes(patt);
    /// let bytes = &[0x00,0x01,0xFE,0xEE,0xEE,0xED,0xEF];
    /// assert!(m.matches_bytes(bytes));
    /// assert_eq!(&bytes[m.range()], &[0xFE,0xEE,0xEE,0xED]);
    /// ```
    pub fn matches_bytes(&mut self, s: &[u8]) -> bool {
        self.n_match = str_match(s, self.patt, &mut self.matches).unwrap();
        self.n_match > 0
    }

    /// Match a string with a pattern
    ///
    /// ```ignore
    /// let mut m = lua_patterns2::LuaPattern::new("(%a+) one");
    /// let text = " hello one two";
    /// assert!(m.matches(text));
    /// ```
    pub fn matches(&mut self, text: &str) -> bool {
        self.matches_bytes(text.as_bytes())
    }

    /// Match a string, returning first capture if successful
    ///
    /// ```ignore
    /// let mut m = lua_patterns2::LuaPattern::new("OK%s+(%d+)");
    /// let res = m.match_maybe("and that's OK 400 to you");
    /// assert_eq!(res, Some("400"));
    /// ```
    pub fn match_maybe<'t>(&mut self, text: &'t str) -> Option<&'t str> {
        if self.matches(text) {
            Some(&text[self.first_capture()])
        } else {
            None
        }
    }

    /// Match a string, returning first two explicit captures if successful
    ///
    /// ```ignore
    /// let mut p = lua_patterns2::LuaPattern::new("%s*(%d+)%s+(%S+)");
    /// let (int,rest) = p.match_maybe_2(" 233   hello dolly").unwrap();
    /// assert_eq!(int,"233");
    /// assert_eq!(rest,"hello");
    /// ```
    pub fn match_maybe_2<'t>(&mut self, text: &'t str) -> Option<(&'t str, &'t str)> {
        if self.matches(text) {
            let cc = self.match_captures(text);
            if cc.num_matches() != 3 {
                return None;
            }
            Some((cc.get(1), cc.get(2)))
        } else {
            None
        }
    }

    /// Match a string, returning first three explicit captures if successful
    ///
    /// ```ignore
    /// let mut p = lua_patterns2::LuaPattern::new("(%d+)/(%d+)/(%d+)");
    /// let (y,m,d) = p.match_maybe_3("2017/11/10").unwrap();
    /// assert_eq!(y,"2017");
    /// assert_eq!(m,"11");
    /// assert_eq!(d,"10");
    /// ```
    pub fn match_maybe_3<'t>(&mut self, text: &'t str) -> Option<(&'t str, &'t str, &'t str)> {
        if self.matches(text) {
            let cc = self.match_captures(text);
            if cc.num_matches() != 4 {
                return None;
            }
            Some((cc.get(1), cc.get(2), cc.get(3)))
        } else {
            None
        }
    }

    /// Match and collect all captures as a vector of string slices
    ///
    /// ```ignore
    /// let mut m = lua_patterns2::LuaPattern::new("(one).+");
    /// assert_eq!(m.captures(" one two"), &["one two","one"]);
    /// ```
    pub fn captures<'b>(&mut self, text: &'b str) -> Vec<&'b str> {
        let mut res = Vec::new();
        self.capture_into(text, &mut res);
        res
    }

    /// Match and collect all captures as a vector of string slices
    ///
    /// ```ignore
    /// let mut m = lua_patterns2::LuaPattern::new("(one).+");
    /// assert_eq!(m.captures(" one two"), &["one two","one"]);
    /// ```
    #[cfg(feature = "heapless")]
    pub fn captures_heapless<'b, const N: usize>(
        &mut self,
        text: &'b str,
    ) -> PartialResult<heapless::Vec<&'b str, N>> {
        let mut res = heapless::Vec::new();
        match self.capture_into_heapless(text, &mut res) {
            Ok(_) => Ok(res),
            Err(_) => Err(res),
        }
    }

    /// A convenient way to access the captures with no allocation
    ///
    /// ```ignore
    /// let text = "  hello one";
    /// let mut m = lua_patterns2::LuaPattern::new("(%S+) one");
    /// if m.matches(text) {
    ///     let cc = m.match_captures(text);
    ///     assert_eq!(cc.get(0), "hello one");
    ///     assert_eq!(cc.get(1), "hello");
    /// }
    /// ```
    pub fn match_captures<'b, 'c>(&'c self, text: &'b str) -> Captures<'a, 'b, 'c> {
        Captures {
            m: self,
            text: text,
        }
    }

    /// Match and collect all captures into the provided vector.
    ///
    /// ```ignore
    /// let text = "  hello one";
    /// let mut m = lua_patterns2::LuaPattern::new("(%S+) one");
    /// let mut v = Vec::new();
    /// if m.capture_into(text,&mut v) {
    ///     assert_eq!(v, &["hello one","hello"]);
    /// }
    /// ```
    pub fn capture_into<'b>(&mut self, text: &'b str, vec: &mut Vec<&'b str>) -> bool {
        self.matches(text);
        vec.clear();
        for i in 0..self.n_match {
            vec.push(&text[self.capture(i)]);
        }
        self.n_match > 0
    }

    /// Match and collect all captures into the provided vector.
    ///
    /// ```ignore
    /// let text = "  hello one";
    /// let mut m = lua_patterns2::LuaPattern::new("(%S+) one");
    /// let mut v = heapless::Vec::new();
    /// if m.capture_into_heapless(text,&mut v).unwrap() {
    ///     assert_eq!(v, &["hello one","hello"]);
    /// }
    /// ```
    #[cfg(feature = "heapless")]
    pub fn capture_into_heapless<'b, const N: usize>(
        &mut self,
        text: &'b str,
        vec: &mut heapless::Vec<&'b str, N>,
    ) -> Result<bool, ()> {
        self.matches(text);
        vec.clear();
        for i in 0..self.n_match {
            vec.push(&text[self.capture(i)]).or(Err(()))?;
        }
        Ok(self.n_match > 0)
    }

    /// The full match (same as `capture(0)`)
    pub fn range(&self) -> ops::Range<usize> {
        self.capture(0)
    }

    /// Get the nth capture of the match.
    ///
    /// ```ignore
    /// let mut m = lua_patterns2::LuaPattern::new("(%a+) one");
    /// let text = " hello one two";
    /// assert!(m.matches(text));
    /// assert_eq!(m.capture(0),1..10);
    /// assert_eq!(m.capture(1),1..6);
    /// ```
    pub fn capture(&self, i: usize) -> ops::Range<usize> {
        ops::Range {
            start: self.matches[i].start as usize,
            end: self.matches[i].end as usize,
        }
    }

    /// Number of captures from the most recent successful match, including the full match.
    pub fn num_matches(&self) -> usize {
        self.n_match
    }

    /// Get the 'first' capture of the match
    ///
    /// If there are no matches, this is the same as `range`,
    /// otherwise it's `capture(1)`
    pub fn first_capture(&self) -> ops::Range<usize> {
        let idx = if self.n_match > 1 { 1 } else { 0 };
        self.capture(idx)
    }

    /// An iterator over all matches in a string.
    ///
    /// The matches are returned as string slices; if there are no
    /// captures the full match is used, otherwise the first capture.
    /// That is, this example will also work with the pattern "(%S+)".
    ///
    /// ```ignore
    /// let mut m = lua_patterns2::LuaPattern::new("%S+");
    /// let split: Vec<_> = m.gmatch("dog  cat leopard wolf").collect();
    /// assert_eq!(split,&["dog","cat","leopard","wolf"]);
    /// ```
    pub fn gmatch<'b, 'c>(&'c mut self, text: &'b str) -> GMatch<'a, 'b, 'c> {
        GMatch {
            m: self,
            text: text,
        }
    }

    /// An iterator over all captures in a string.
    ///
    /// The matches are returned as captures; this is a _streaming_
    /// iterator, so don't try to collect the captures directly; extract
    /// the string slices using `get`.
    ///
    /// ```ignore
    /// let mut m = lua_patterns2::LuaPattern::new("(%S)%S+");
    /// let split: Vec<_> = m.gmatch_captures("dog  cat leopard wolf")
    ///       .map(|cc| cc.get(1)).collect();
    /// assert_eq!(split,&["d","c","l","w"]);
    /// ```
    pub fn gmatch_captures<'b, 'c>(&'c mut self, text: &'b str) -> GMatchCaptures<'a, 'b, 'c> {
        GMatchCaptures {
            m: self,
            text: text,
        }
    }

    /// An iterator over all matches in a slice of bytes.
    ///
    /// ```ignore
    /// let bytes = &[0xAA,0x01,0x01,0x03,0xBB,0x01,0x01,0x01];
    /// let patt = &[0x01,b'+'];
    /// let mut m = lua_patterns2::LuaPattern::from_bytes(patt);
    /// let mut iter = m.gmatch_bytes(bytes);
    /// assert_eq!(iter.next().unwrap(), &[0x01,0x01]);
    /// assert_eq!(iter.next().unwrap(), &[0x01,0x01,0x01]);
    /// assert_eq!(iter.next(), None);
    /// ```
    pub fn gmatch_bytes<'b>(&'a mut self, bytes: &'b [u8]) -> GMatchBytes<'a, 'b> {
        GMatchBytes {
            m: self,
            bytes: bytes,
        }
    }

    /// Globally substitute all matches with a replacement
    /// provided by a function of the captures.
    ///
    /// ```ignore
    /// let mut m = lua_patterns2::LuaPattern::new("%$(%S+)");
    /// let res = m.gsub_with("hello $dolly you're so $fine!",
    ///     |cc| cc.get(1).to_uppercase()
    /// );
    /// assert_eq!(res, "hello DOLLY you're so FINE!");
    /// ```
    pub fn gsub_with<F>(&mut self, text: &str, lookup: F) -> String
    where
        F: Fn(Captures) -> String,
    {
        let mut slice = text;
        let mut res = String::new();
        while self.matches(slice) {
            // full range of match
            let all = self.range();
            // append everything up to match
            res.push_str(&slice[0..all.start]);
            let captures = Captures {
                m: self,
                text: slice,
            };
            let repl = lookup(captures);
            res.push_str(&repl);
            slice = &slice[all.end..];
        }
        res.push_str(slice);
        res
    }

    /// Globally substitute all matches with a replacement
    /// provided by a function of the captures.
    ///
    /// If there isn't enough space the Err variant will be returned with a partial result.
    ///
    /// ```ignore
    /// let mut m = lua_patterns2::LuaPattern::new("%$(%S+)");
    /// let res = m.gsub_with_heapless("hello $dolly you're so $fine!",
    ///     |cc| cc.get(1).to_uppercase()
    /// ).unwrap();
    /// assert_eq!(res, "hello DOLLY you're so FINE!");
    /// ```
    #[cfg(feature = "heapless")]
    pub fn gsub_with_heapless<F, const N: usize, const M: usize>(
        &mut self,
        text: &str,
        lookup: F,
    ) -> PartialResult<heapless::String<N>>
    where
        F: Fn(Captures) -> heapless::String<M>,
    {
        let mut slice = text;
        let mut res = heapless::String::new();
        while self.matches(slice) {
            // full range of match
            let all = self.range();
            // append everything up to match
            if let Err(_) = res.push_str(&slice[0..all.start]) {
                return Err(res);
            }
            let captures = Captures {
                m: self,
                text: slice,
            };
            let repl = lookup(captures);
            if let Err(_) = res.push_str(&repl) {
                return Err(res);
            }
            slice = &slice[all.end..];
        }
        if let Err(_) = res.push_str(slice) {
            return Err(res);
        }
        Ok(res)
    }

    /// Globally substitute all _byte_ matches with a replacement
    /// provided by a function of the captures.
    ///
    /// ```ignore
    /// let bytes = &[0xAA,0x01,0x02,0x03,0xBB];
    /// let patt = &[0x01,0x02];
    /// let mut m = lua_patterns2::LuaPattern::from_bytes(patt);
    /// let res = m.gsub_bytes_with(bytes,|cc| vec![0xFF]);
    /// assert_eq!(res, &[0xAA,0xFF,0x03,0xBB]);
    /// ```
    pub fn gsub_bytes_with<F>(&mut self, bytes: &[u8], lookup: F) -> Vec<u8>
    where
        F: Fn(ByteCaptures) -> Vec<u8>,
    {
        let mut slice = bytes;
        let mut res = Vec::new();
        while self.matches_bytes(slice) {
            let all = self.range();
            let capture = &slice[0..all.start];
            res.extend_from_slice(capture);
            let captures = ByteCaptures {
                m: self,
                bytes: slice,
            };
            let repl = lookup(captures);
            res.extend(repl);
            slice = &slice[all.end..];
        }
        res.extend_from_slice(slice);
        res
    }

    /// Globally substitute all _byte_ matches with a replacement
    /// provided by a function of the captures.
    ///
    /// If there isn't enough space the Err variant will be returned with a partial result.
    ///
    /// ```ignore
    /// let bytes = &[0xAA,0x01,0x02,0x03,0xBB];
    /// let patt = &[0x01,0x02];
    /// let mut m = lua_patterns2::LuaPattern::from_bytes(patt);
    /// let res = m.gsub_bytes_with_heapless(bytes,|cc| vec![0xFF]);
    /// assert_eq!(res, &[0xAA,0xFF,0x03,0xBB]);
    /// ```
    #[cfg(feature = "heapless")]
    pub fn gsub_bytes_with_heapless<F, const N: usize, const M: usize>(
        &mut self,
        bytes: &[u8],
        lookup: F,
    ) -> PartialResult<heapless::Vec<u8, N>>
    where
        F: Fn(ByteCaptures) -> heapless::Vec<u8, M>,
    {
        let mut slice = bytes;
        let mut res = heapless::Vec::new();
        while self.matches_bytes(slice) {
            let all = self.range();
            let capture = &slice[0..all.start];
            if let Err(_) = res.extend_from_slice(capture) {
                return Err(res);
            }
            let captures = ByteCaptures {
                m: self,
                bytes: slice,
            };
            let repl = lookup(captures);
            res.extend(repl);
            slice = &slice[all.end..];
        }
        if let Err(_) = res.extend_from_slice(slice) {
            return Err(res);
        }
        Ok(res)
    }
}

/// Low-overhead convenient access to string match captures
// note: there are three borrows going on here.
// The lifetime 'a is for the _pattern_, the lifetime 'b is
// for the _source string_, and 'c is for the reference to LuaPattern
// And the LuaPattern reference cannot live longer than the pattern reference
pub struct Captures<'a, 'b, 'c>
where
    'a: 'c,
{
    m: &'c LuaPattern<'a>,
    text: &'b str,
}

impl<'a, 'b, 'c> Captures<'a, 'b, 'c> {
    /// get the capture as a string slice
    pub fn get(&self, i: usize) -> &'b str {
        &self.text[self.m.capture(i)]
    }

    /// number of matches
    pub fn num_matches(&self) -> usize {
        self.m.n_match
    }
}

/// Low-overhead convenient access to byte match captures
pub struct ByteCaptures<'a, 'b> {
    m: &'a LuaPattern<'a>,
    bytes: &'b [u8],
}

impl<'a, 'b> ByteCaptures<'a, 'b> {
    /// get the capture as a byte slice
    pub fn get(&self, i: usize) -> &'b [u8] {
        &self.bytes[self.m.capture(i)]
    }

    /// number of matches
    pub fn num_matches(&self) -> usize {
        self.m.n_match
    }
}

/// Iterator for all string slices from `gmatch`
// note lifetimes as for Captures above!
pub struct GMatch<'a, 'b, 'c>
where
    'a: 'c,
{
    m: &'c mut LuaPattern<'a>,
    text: &'b str,
}

impl<'a, 'b, 'c> Iterator for GMatch<'a, 'b, 'c> {
    type Item = &'b str;

    fn next(&mut self) -> Option<Self::Item> {
        if !self.m.matches(self.text) {
            None
        } else {
            let slice = &self.text[self.m.first_capture()];
            self.text = &self.text[self.m.range().end..];
            Some(slice)
        }
    }
}

/// Unsafe version of Captures, needed for gmatch_captures
// It's unsafe because the lifetime only depends on the original
// text, not the borrowed matches.
pub struct CapturesUnsafe<'b> {
    matches: *const LuaMatch,
    text: &'b str,
}

impl<'b> CapturesUnsafe<'b> {
    /// get the capture as a string slice
    pub fn get(&self, i: usize) -> &'b str {
        unsafe {
            let p = self.matches.offset(i as isize);
            let range = ops::Range {
                start: (*p).start as usize,
                end: (*p).end as usize,
            };
            &self.text[range]
        }
    }
}

/// Streaming iterator for all captures from `gmatch_captures`
// lifetimes as for Captures above!
// 'a is pattern, 'b is text, 'c is ref to LuaPattern
pub struct GMatchCaptures<'a, 'b, 'c>
where
    'a: 'c,
{
    m: &'c mut LuaPattern<'a>,
    text: &'b str,
}

impl<'a, 'b, 'c> Iterator for GMatchCaptures<'a, 'b, 'c>
where
    'a: 'c,
{
    type Item = CapturesUnsafe<'b>;

    fn next(&mut self) -> Option<Self::Item> {
        if !self.m.matches(self.text) {
            None
        } else {
            let split = self.text.split_at(self.m.range().end);
            self.text = split.1;
            let match_ptr: *const LuaMatch = self.m.matches.as_ptr();
            Some(CapturesUnsafe {
                matches: match_ptr,
                text: split.0,
            })
        }
    }
}

/// Iterator for all byte slices from `gmatch_bytes`
pub struct GMatchBytes<'a, 'b> {
    m: &'a mut LuaPattern<'a>,
    bytes: &'b [u8],
}

impl<'a, 'b> Iterator for GMatchBytes<'a, 'b> {
    type Item = &'b [u8];

    fn next(&mut self) -> Option<Self::Item> {
        if !self.m.matches_bytes(self.bytes) {
            None
        } else {
            let slice = &self.bytes[self.m.first_capture()];
            self.bytes = &self.bytes[self.m.range().end..];
            Some(slice)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_and_matching() {
        let mut m = LuaPattern::new("(one).+");
        assert_eq!(m.captures(" one two"), &["one two", "one"]);
        let empty: &[&str] = &[];
        assert_eq!(m.captures("four"), empty);

        assert_eq!(m.matches("one dog"), true);
        assert_eq!(m.matches("dog one "), true);
        assert_eq!(m.matches("dog one"), false);

        let text = "one dog";
        let mut m = LuaPattern::new("^(%a+)");
        assert_eq!(m.matches(text), true);
        assert_eq!(&text[m.capture(1)], "one");
        assert_eq!(m.matches(" one dog"), false);

        // captures without allocation
        m.matches(text);
        let captures = m.match_captures(text);
        assert_eq!(captures.get(0), "one");
        assert_eq!(captures.get(1), "one");

        let mut m = LuaPattern::new("(%S+)%s*=%s*(.+)");

        //  captures as Vec
        let cc = m.captures(" hello= bonzo dog");
        assert_eq!(cc[0], "hello= bonzo dog");
        assert_eq!(cc[1], "hello");
        assert_eq!(cc[2], "bonzo dog");
    }

    #[test]
    fn multiple_captures() {
        let mut p = LuaPattern::new("%s*(%d+)%s+(%S+)");
        let (int, rest) = p.match_maybe_2(" 233   hello dolly").unwrap();
        assert_eq!(int, "233");
        assert_eq!(rest, "hello");
    }

    #[test]
    fn gmatch() {
        let mut m = LuaPattern::new("%a+");
        let mut iter = m.gmatch("one two three");
        assert_eq!(iter.next(), Some("one"));
        assert_eq!(iter.next(), Some("two"));
        assert_eq!(iter.next(), Some("three"));
        assert_eq!(iter.next(), None);

        let mut m = LuaPattern::new("(%a+)");
        let mut iter = m.gmatch("one two three");
        assert_eq!(iter.next(), Some("one"));
        assert_eq!(iter.next(), Some("two"));
        assert_eq!(iter.next(), Some("three"));
        assert_eq!(iter.next(), None);

        let mut m = LuaPattern::new("(%a+)");
        let mut iter = m.gmatch_captures("one two three");
        assert_eq!(iter.next().unwrap().get(1), "one");
        assert_eq!(iter.next().unwrap().get(1), "two");
        assert_eq!(iter.next().unwrap().get(1), "three");
    }

    #[test]
    fn gsub() {
        use std::collections::HashMap;

        let mut m = LuaPattern::new("%$(%S+)");
        let res = m.gsub_with("hello $dolly you're so $fine!", |cc| {
            cc.get(1).to_uppercase()
        });
        assert_eq!(res, "hello DOLLY you're so FINE!");

        let mut map = HashMap::new();
        map.insert("dolly", "baby");
        map.insert("fine", "cool");
        map.insert("good-looking", "pretty");

        let mut m = LuaPattern::new("%$%((.-)%)");
        let res = m.gsub_with(
            "hello $(dolly) you're so $(fine) and $(good-looking)",
            |cc| map.get(cc.get(1)).unwrap_or(&"?").to_string(),
        );
        assert_eq!(res, "hello baby you're so cool and pretty");

        let mut m = LuaPattern::new("%s+");
        let res = m.gsub("hello dolly you're so fine", "");
        assert_eq!(res, "hellodollyyou'resofine");

        let mut m = LuaPattern::new("(%S+)%s*=%s*(%S+);%s*");
        let res = m.gsub("a=2; b=3; c = 4;", "'%2':%1 ");
        assert_eq!(res, "'2':a '3':b '4':c ");
    }

    #[test]
    fn bad_patterns() {
        let bad = [
            ("bonzo %", PatternError::MalformedPattern("ends with '%'")),
            ("bonzo (dog%(", PatternError::UnfinishedCapture),
            ("alles [%a%[", PatternError::MalformedPattern("missing ']'")),
            ("bonzo (dog (cat)", PatternError::UnfinishedCapture),
            ("frodo %f[%A", PatternError::MalformedPattern("missing ']'")),
            (
                "frodo (1) (2(3)%2)%1",
                PatternError::InvalidCaptureIndex(Some(1)),
            ),
        ];
        for p in bad.iter() {
            let res = LuaPattern::new_try(p.0);
            if let Err(e) = res {
                assert_eq!(e, p.1);
            } else {
                panic!("false positive");
            }
        }
    }
}
