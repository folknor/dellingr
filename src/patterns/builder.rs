//! Create pattern byte sequences (that can be used to construct [LuaPattern])
//! dynamically at runtime using a builder API.

use std::format;
use std::string::String;
use std::vec::Vec;

use super::LuaPattern;

/// Build a byte Lua pattern, optionally escaping 'magic' characters
pub struct LuaPatternBuilder {
    bytes: Vec<u8>,
}

impl LuaPatternBuilder {
    /// Create a new Lua pattern builder
    pub fn new() -> LuaPatternBuilder {
        LuaPatternBuilder { bytes: Vec::new() }
    }

    /// Add unescaped characters from a string
    ///
    /// ```ignore
    /// let patt = lua_patterns2::LuaPatternBuilder::new()
    ///     .text("(boo)")
    ///     .build();
    /// assert_eq!(std::str::from_utf8(&patt).unwrap(), "(boo)");
    /// ```
    pub fn text(&mut self, s: &str) -> &mut Self {
        self.bytes.extend_from_slice(s.as_bytes());
        self
    }

    /// Add unescaped characters from lines
    ///
    /// This looks for first non-whitespace run in each line,
    /// useful for spreading patterns out and commmenting them.
    /// Works with patterns that use '%s' religiously!
    ///
    /// ```ignore
    /// let patt = lua_patterns2::LuaPatternBuilder::new()
    ///     .text_lines("
    ///       hello-dolly
    ///       you-are-fine  # comment
    ///       cool
    ///      ")
    ///     .build();
    /// assert_eq!(std::str::from_utf8(&patt).unwrap(),
    ///   "hello-dollyyou-are-finecool");
    /// ```
    pub fn text_lines(&mut self, lines: &str) -> &mut Self {
        let mut text = String::new();
        for line in lines.lines() {
            if let Some(first) = line.split_whitespace().next() {
                text.push_str(first);
            }
        }
        self.text(&text)
    }

    /// Add escaped bytes from a slice
    ///
    /// ```ignore
    /// let patt = lua_patterns2::LuaPatternBuilder::new()
    ///     .text("^")
    ///     .bytes(b"^") // magic character!
    ///     .build();
    /// assert_eq!(std::str::from_utf8(&patt).unwrap(), "^%^");
    /// ```
    pub fn bytes(&mut self, b: &[u8]) -> &mut Self {
        let mut m = LuaPattern::new("[%-%.%+%[%]%(%)%$%^%%%?%*]");
        let bb = m.gsub_bytes_with(b, |cc| {
            let mut res = Vec::new();
            res.push(b'%');
            res.push(cc.get(0)[0]);
            res
        });
        self.bytes.extend(bb);
        self
    }

    /// Add escaped bytes from hex string
    ///
    /// This consists of adjacent pairs of hex digits.
    ///
    /// ```ignore
    /// let patt = lua_patterns2::LuaPatternBuilder::new()
    ///     .text("^")
    ///     .bytes_as_hex("5E") // which is ASCII '^'
    ///     .build();
    /// assert_eq!(std::str::from_utf8(&patt).unwrap(), "^%^");
    /// ```
    pub fn bytes_as_hex(&mut self, bs: &str) -> &mut Self {
        let bb = LuaPatternBuilder::hex_to_bytes(bs);
        self.bytes(&bb)
    }

    /// Create the pattern
    pub fn build(&mut self) -> Vec<u8> {
        let mut v = Vec::new();
        core::mem::swap(&mut self.bytes, &mut v);
        v
    }

    /// Utility to create a vector of bytes from a hex string
    ///
    /// ```ignore
    /// let bb = lua_patterns2::LuaPatternBuilder::hex_to_bytes("AEFE00FE");
    /// assert_eq!(bb, &[0xAE,0xFE,0x00,0xFE]);
    /// ```
    pub fn hex_to_bytes(s: &str) -> Vec<u8> {
        let mut m = LuaPattern::new("%x%x");
        m.gmatch(s)
            .map(|pair| u8::from_str_radix(pair, 16).unwrap())
            .collect()
    }

    /// Utility to create a hex string from a slice of bytes
    ///
    /// ```ignore
    /// let hex = lua_patterns2::LuaPatternBuilder::bytes_to_hex(&[0xAE,0xFE,0x00,0xFE]);
    /// assert_eq!(hex,"AEFE00FE");
    ///
    /// ```
    pub fn bytes_to_hex(s: &[u8]) -> String {
        s.iter().map(|b| format!("{:02X}", b)).collect()
    }
}
