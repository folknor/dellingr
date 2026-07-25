//! Compiled, byte-oriented Lua patterns.

use core::result;
use std::collections::BTreeMap;

use super::errors::*;

pub(super) const LUA_MAXCAPTURES: usize = 32;
pub(super) const LUA_MAXMATCHES: usize = LUA_MAXCAPTURES + 1;
const MAXCCALLS: usize = 200;
const L_ESC: u8 = b'%';

type Result<T> = result::Result<T, PatternError>;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum LuaCapture {
    Bytes {
        start: usize,
        end: usize,
    },
    /// Zero-based byte offset within the complete subject.
    Position(usize),
}

#[derive(Copy, Clone)]
enum CapLen {
    Len(usize),
    Unfinished,
    Position,
}

#[derive(Copy, Clone)]
struct Capture {
    init: usize,
    len: CapLen,
}

#[derive(Copy, Clone)]
struct ClassId(usize);

#[derive(Clone)]
struct ByteClass([u64; 4]);

impl ByteClass {
    fn empty() -> Self {
        Self([0; 4])
    }
    fn insert(&mut self, byte: u8) {
        self.0[usize::from(byte) / 64] |= 1 << (byte % 64);
    }
    fn contains(&self, byte: u8) -> bool {
        self.0[usize::from(byte) / 64] & (1 << (byte % 64)) != 0
    }
    fn invert(&mut self) {
        for word in &mut self.0 {
            *word = !*word;
        }
    }
}

#[derive(Copy, Clone)]
enum Repeat {
    One,
    Optional,
    ZeroOrMoreGreedy,
    OneOrMoreGreedy,
    ZeroOrMoreMinimal,
}

#[derive(Copy, Clone)]
enum Item {
    Atom { class: ClassId, repeat: Repeat },
    Balance { open: u8, close: u8 },
    Frontier { class: ClassId },
    Backref { slot: u8 },
    CaptureStart { slot: u8 },
    CaptureEnd { slot: u8 },
    PositionCapture { slot: u8 },
    EndAnchor,
}

pub(super) struct CompiledPattern {
    items: Vec<Item>,
    classes: Vec<ByteClass>,
    anchored: bool,
    captures: usize,
}

struct Compiler<'a> {
    pattern: &'a [u8],
    pos: usize,
    items: Vec<Item>,
    classes: Vec<ByteClass>,
    /// Deduplicates identical classes so a pattern like `%a%a%a` or a long
    /// literal run stores one `ByteClass` per *distinct* class, not per item.
    /// `BTreeMap` rather than a hash map: iteration and identity must stay
    /// deterministic.
    class_ids: BTreeMap<[u64; 4], ClassId>,
    capture_stack: [usize; LUA_MAXCAPTURES],
    stack_len: usize,
    captures: usize,
}

impl<'a> Compiler<'a> {
    fn new(pattern: &'a [u8]) -> Self {
        Self {
            pattern,
            pos: 0,
            items: Vec::new(),
            classes: Vec::new(),
            class_ids: BTreeMap::new(),
            capture_stack: [0; LUA_MAXCAPTURES],
            stack_len: 0,
            captures: 0,
        }
    }

    /// Intern a class, returning the id of an identical existing one if there
    /// is one. Every atom - literal byte, `.`, `%a`-style class and bracket
    /// class alike - is compiled to one of these, so matching a single item is
    /// always one bitset test with no per-item branch on atom kind.
    fn intern_class(&mut self, class: ByteClass) -> ClassId {
        if let Some(&id) = self.class_ids.get(&class.0) {
            return id;
        }
        let id = ClassId(self.classes.len());
        self.class_ids.insert(class.0, id);
        self.classes.push(class);
        id
    }

    fn literal_class(&mut self, byte: u8) -> ClassId {
        let mut result = ByteClass::empty();
        result.insert(byte);
        self.intern_class(result)
    }

    fn any_class(&mut self) -> ClassId {
        let mut result = ByteClass::empty();
        result.invert();
        self.intern_class(result)
    }

    fn class_for_escape(&mut self, class: u8) -> ClassId {
        let mut result = ByteClass::empty();
        for byte in u8::MIN..=u8::MAX {
            if match_class(byte, class) {
                result.insert(byte);
            }
        }
        self.intern_class(result)
    }

    fn bracket_class(&mut self) -> Result<ClassId> {
        let mut invert = false;
        if self.pattern.get(self.pos) == Some(&b'^') {
            invert = true;
            self.pos += 1;
        }
        let class_start = self.pos;
        let mut scan = self.pos;
        let close = loop {
            let Some(&byte) = self.pattern.get(scan) else {
                return Err(PatternError::MalformedPattern(
                    MalformedPattern::MissingBracket,
                ));
            };
            scan += 1;
            if byte == L_ESC && scan < self.pattern.len() {
                scan += 1;
            }
            if scan >= self.pattern.len() {
                return Err(PatternError::MalformedPattern(
                    MalformedPattern::MissingBracket,
                ));
            }
            if self.pattern[scan] == b']' {
                break scan;
            }
        };
        if class_start == close {
            return Err(PatternError::MalformedPattern(
                MalformedPattern::MissingBracket,
            ));
        }
        // The three arms below are mutually exclusive and ordered exactly as
        // reference `matchbracketclass`. Flattening them changes which bytes a
        // class contains: an escaped item can never also open a range, and a
        // range never separately contributes its start byte - so `[z-a]`
        // matches nothing rather than `z`, `[%a-z]` is the alphabetic class
        // plus a literal `-`, and `[%--a]` is exactly `-` and `a`.
        let mut class = ByteClass::empty();
        while self.pos < close {
            let byte = self.pattern[self.pos];
            if byte == L_ESC && self.pos + 1 < close {
                let escaped = self.pattern[self.pos + 1];
                self.pos += 2;
                for candidate in u8::MIN..=u8::MAX {
                    if match_class(candidate, escaped) {
                        class.insert(candidate);
                    }
                }
            } else if self.pos + 2 < close && self.pattern[self.pos + 1] == b'-' {
                let end = self.pattern[self.pos + 2];
                self.pos += 3;
                // A descending range such as `[z-a]` is empty, and inserting
                // nothing is what makes it match nothing.
                for candidate in byte..=end {
                    class.insert(candidate);
                }
            } else {
                class.insert(byte);
                self.pos += 1;
            }
        }
        self.pos = close + 1;
        if invert {
            class.invert();
        }
        Ok(self.intern_class(class))
    }

    fn compile(mut self) -> Result<CompiledPattern> {
        let anchored = self.pattern.first() == Some(&b'^');
        if anchored {
            self.pos = 1;
        }
        while self.pos < self.pattern.len() {
            let byte = self.pattern[self.pos];
            self.pos += 1;
            match byte {
                b'(' => {
                    if self.captures == LUA_MAXCAPTURES {
                        return Err(PatternError::TooManyCaptures);
                    }
                    let slot = self.captures;
                    self.captures += 1;
                    if self.pattern.get(self.pos) == Some(&b')') {
                        self.pos += 1;
                        self.items.push(Item::PositionCapture { slot: slot as u8 });
                    } else {
                        self.capture_stack[self.stack_len] = slot;
                        self.stack_len += 1;
                        self.items.push(Item::CaptureStart { slot: slot as u8 });
                    }
                }
                b')' => {
                    if self.stack_len == 0 {
                        return Err(PatternError::InvalidPatternCapture);
                    }
                    self.stack_len -= 1;
                    self.items.push(Item::CaptureEnd {
                        slot: self.capture_stack[self.stack_len] as u8,
                    });
                }
                b'$' if self.pos == self.pattern.len() => self.items.push(Item::EndAnchor),
                L_ESC => {
                    let Some(&escaped) = self.pattern.get(self.pos) else {
                        return Err(PatternError::MalformedPattern(
                            MalformedPattern::EndsWithPercent,
                        ));
                    };
                    self.pos += 1;
                    match escaped {
                        b'b' => {
                            let Some(&open) = self.pattern.get(self.pos) else {
                                return Err(PatternError::MalformedPattern(
                                    MalformedPattern::MissingBalancedArguments,
                                ));
                            };
                            let Some(&close) = self.pattern.get(self.pos + 1) else {
                                return Err(PatternError::MalformedPattern(
                                    MalformedPattern::MissingBalancedArguments,
                                ));
                            };
                            self.pos += 2;
                            self.items.push(Item::Balance { open, close });
                        }
                        b'f' => {
                            if self.pattern.get(self.pos) != Some(&b'[') {
                                return Err(PatternError::MalformedPattern(
                                    MalformedPattern::MissingFrontierBracket,
                                ));
                            }
                            self.pos += 1;
                            let class = self.bracket_class()?;
                            self.items.push(Item::Frontier { class });
                        }
                        b'0'..=b'9' => {
                            // `%1`..`%9` are one-based, so `%0` yields -1 and is
                            // always invalid. The short-circuit order keeps
                            // every `as usize` below on a nonnegative value.
                            let index = escaped as i8 - b'1' as i8;
                            let resolved = index >= 0
                                && (index as usize) < self.captures
                                && !self.capture_stack[..self.stack_len]
                                    .contains(&(index as usize));
                            if !resolved {
                                return Err(PatternError::InvalidCaptureIndex(Some(index)));
                            }
                            self.items.push(Item::Backref { slot: index as u8 });
                        }
                        _ => {
                            let class = self.class_for_escape(escaped);
                            self.push_atom(class);
                        }
                    }
                }
                b'[' => {
                    let class = self.bracket_class()?;
                    self.push_atom(class);
                }
                b'.' => {
                    let class = self.any_class();
                    self.push_atom(class);
                }
                _ => {
                    let class = self.literal_class(byte);
                    self.push_atom(class);
                }
            }
        }
        if self.stack_len != 0 {
            return Err(PatternError::UnfinishedCapture);
        }
        Ok(CompiledPattern {
            items: self.items,
            classes: self.classes,
            anchored,
            captures: self.captures,
        })
    }

    fn push_atom(&mut self, class: ClassId) {
        let repeat = match self.pattern.get(self.pos) {
            Some(b'?') => Repeat::Optional,
            Some(b'*') => Repeat::ZeroOrMoreGreedy,
            Some(b'+') => Repeat::OneOrMoreGreedy,
            Some(b'-') => Repeat::ZeroOrMoreMinimal,
            _ => Repeat::One,
        };
        if !matches!(repeat, Repeat::One) {
            self.pos += 1;
        }
        self.items.push(Item::Atom { class, repeat });
    }
}

fn match_class(ch: u8, class: u8) -> bool {
    let result = match class.to_ascii_lowercase() {
        b'a' => ch.is_ascii_alphabetic(),
        b'c' => ch.is_ascii_control(),
        b'd' => ch.is_ascii_digit(),
        b'g' => ch.is_ascii_graphic(),
        b'l' => ch.is_ascii_lowercase(),
        b'p' => ch.is_ascii_punctuation(),
        b's' => crate::numeral::is_lua_whitespace(ch),
        b'u' => ch.is_ascii_uppercase(),
        b'w' => ch.is_ascii_alphanumeric(),
        b'x' => ch.is_ascii_hexdigit(),
        b'z' => ch == 0,
        _ => return class == ch,
    };
    if class.is_ascii_lowercase() {
        result
    } else {
        !result
    }
}

struct MatchState<'a> {
    subject: &'a [u8],
    pattern: &'a CompiledPattern,
    captures: [Capture; LUA_MAXCAPTURES],
}

impl<'a> MatchState<'a> {
    fn new(subject: &'a [u8], pattern: &'a CompiledPattern) -> Self {
        Self {
            subject,
            pattern,
            captures: [Capture {
                init: 0,
                len: CapLen::Len(0),
            }; LUA_MAXCAPTURES],
        }
    }
    fn singlematch(&self, pos: usize, class: ClassId) -> bool {
        let Some(&byte) = self.subject.get(pos) else {
            return false;
        };
        self.pattern.classes[class.0].contains(byte)
    }
    fn match_balance(&self, pos: usize, open: u8, close: u8) -> Option<usize> {
        if self.subject.get(pos) != Some(&open) {
            return None;
        }
        let mut depth = 1;
        let mut cursor = pos + 1;
        while let Some(&byte) = self.subject.get(cursor) {
            if byte == close {
                depth -= 1;
                if depth == 0 {
                    return Some(cursor + 1);
                }
            } else if byte == open {
                depth += 1;
            }
            cursor += 1;
        }
        None
    }
    fn recurse(&mut self, pos: usize, item: usize, depth: usize) -> Result<Option<usize>> {
        self.match_at(pos, item, depth + 1)
    }
    fn max_expand(
        &mut self,
        pos: usize,
        item: usize,
        class: ClassId,
        min: usize,
        depth: usize,
    ) -> Result<Option<usize>> {
        let mut end = pos;
        while self.singlematch(end, class) {
            end += 1;
        }
        while end >= pos + min {
            if let Some(result) = self.recurse(end, item + 1, depth)? {
                return Ok(Some(result));
            }
            if end == pos + min {
                break;
            }
            end -= 1;
        }
        Ok(None)
    }
    fn min_expand(
        &mut self,
        mut pos: usize,
        item: usize,
        class: ClassId,
        depth: usize,
    ) -> Result<Option<usize>> {
        loop {
            if let Some(result) = self.recurse(pos, item + 1, depth)? {
                return Ok(Some(result));
            }
            if !self.singlematch(pos, class) {
                return Ok(None);
            }
            pos += 1;
        }
    }
    fn match_at(&mut self, mut pos: usize, mut item: usize, depth: usize) -> Result<Option<usize>> {
        if depth >= MAXCCALLS {
            return Err(PatternError::MatchDepthExceeded);
        }
        loop {
            // Central matcher-step site reserved for the future cost hook.
            let Some(current) = self.pattern.items.get(item).copied() else {
                return Ok(Some(pos));
            };
            match current {
                Item::CaptureStart { slot } => {
                    self.captures[usize::from(slot)] = Capture {
                        init: pos,
                        len: CapLen::Unfinished,
                    };
                    return self.recurse(pos, item + 1, depth);
                }
                Item::CaptureEnd { slot } => {
                    let slot = usize::from(slot);
                    self.captures[slot].len = CapLen::Len(pos - self.captures[slot].init);
                    let result = self.recurse(pos, item + 1, depth)?;
                    if result.is_none() {
                        self.captures[slot].len = CapLen::Unfinished;
                    }
                    return Ok(result);
                }
                Item::PositionCapture { slot } => {
                    self.captures[usize::from(slot)] = Capture {
                        init: pos,
                        len: CapLen::Position,
                    };
                    // Reference routes `()` through `start_capture`, which
                    // recurses, so a position capture costs a depth level just
                    // like `(`. Continuing in this loop instead would let
                    // patterns that reference rejects match here.
                    return self.recurse(pos, item + 1, depth);
                }
                Item::Balance { open, close } => match self.match_balance(pos, open, close) {
                    Some(next) => {
                        pos = next;
                        item += 1;
                    }
                    None => return Ok(None),
                },
                Item::Frontier { class } => {
                    let previous = pos
                        .checked_sub(1)
                        .and_then(|i| self.subject.get(i))
                        .copied()
                        .unwrap_or(0);
                    let current = self.subject.get(pos).copied().unwrap_or(0);
                    if self.pattern.classes[class.0].contains(previous)
                        || !self.pattern.classes[class.0].contains(current)
                    {
                        return Ok(None);
                    }
                    item += 1;
                }
                Item::Backref { slot } => {
                    let capture = self.captures[usize::from(slot)];
                    let len = match capture.len {
                        CapLen::Len(length) => length,
                        CapLen::Position => return Ok(None),
                        CapLen::Unfinished => return Err(PatternError::UnfinishedCapture),
                    };
                    if self.subject.get(capture.init..capture.init + len)
                        != self.subject.get(pos..pos + len)
                    {
                        return Ok(None);
                    }
                    pos += len;
                    item += 1;
                }
                Item::EndAnchor => return Ok((pos == self.subject.len()).then_some(pos)),
                Item::Atom { class, repeat } => match repeat {
                    Repeat::One => {
                        if !self.singlematch(pos, class) {
                            return Ok(None);
                        }
                        pos += 1;
                        item += 1;
                    }
                    Repeat::Optional => {
                        if self.singlematch(pos, class)
                            && let Some(result) = self.recurse(pos + 1, item + 1, depth)?
                        {
                            return Ok(Some(result));
                        }
                        item += 1;
                    }
                    // `*` and `-` that cannot match even once take reference's
                    // depth-free accept-empty transition (`goto init`), so they
                    // must advance in this loop rather than recurse through an
                    // expansion helper. Otherwise a long run of them exhausts
                    // the depth budget on a subject they all match emptily.
                    Repeat::ZeroOrMoreGreedy => {
                        if !self.singlematch(pos, class) {
                            item += 1;
                            continue;
                        }
                        return self.max_expand(pos, item, class, 0, depth);
                    }
                    Repeat::OneOrMoreGreedy => {
                        if !self.singlematch(pos, class) {
                            return Ok(None);
                        }
                        return self.max_expand(pos + 1, item, class, 0, depth);
                    }
                    Repeat::ZeroOrMoreMinimal => {
                        if !self.singlematch(pos, class) {
                            item += 1;
                            continue;
                        }
                        return self.min_expand(pos, item, class, depth);
                    }
                },
            }
        }
    }
    fn captures(
        &self,
        start: usize,
        end: usize,
        out: &mut [LuaCapture; LUA_MAXMATCHES],
    ) -> Result<usize> {
        out[0] = LuaCapture::Bytes { start, end };
        for index in 0..self.pattern.captures {
            out[index + 1] = match self.captures[index].len {
                CapLen::Len(length) => LuaCapture::Bytes {
                    start: self.captures[index].init,
                    end: self.captures[index].init + length,
                },
                CapLen::Position => LuaCapture::Position(self.captures[index].init),
                CapLen::Unfinished => return Err(PatternError::UnfinishedCapture),
            };
        }
        Ok(self.pattern.captures + 1)
    }
}

pub(super) fn compile(pattern: &[u8]) -> Result<CompiledPattern> {
    Compiler::new(pattern).compile()
}

pub(super) fn str_match(
    subject: &[u8],
    pattern: &CompiledPattern,
    init: usize,
    out: &mut [LuaCapture; LUA_MAXMATCHES],
) -> Result<usize> {
    if pattern.items.is_empty() {
        out[0] = LuaCapture::Bytes {
            start: init,
            end: init,
        };
        return Ok(1);
    }
    let mut pos = init;
    loop {
        let mut state = MatchState::new(subject, pattern);
        if let Some(end) = state.match_at(pos, 0, 0)? {
            return state.captures(pos, end, out);
        }
        if pattern.anchored || pos == subject.len() {
            return Ok(0);
        }
        pos += 1;
    }
}
