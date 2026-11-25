//! A function's instruction stream with its register tier.
//!
//! The words are always the canonical instructions, so every reader
//! (serialization, disassembly, cost analysis, numeric-loop recognition, the
//! verifier) sees them as a plain `[Instr]` slice. The tier says how dispatch
//! may index registers: `Wide` indexes a frame window by the full 16-bit
//! field, which is in bounds for any word; `Narrow` promises every
//! register-use field fits a `u8` (`NarrowInstr::new`), so dispatch can index
//! a 256-slot window by the narrowed field.
//!
//! It is a child of `instr` so the view can wrap a checked word without
//! checking it again on every fetch. Both fields are private, so the promise
//! rests on this module and `NarrowInstr::new` alone:
//! - `Wide` is the default and what a built instruction vector becomes, so
//!   parser output, fixtures and edited code are never narrow.
//! - Every mutable access to the words sets `Wide` first, so a narrow claim
//!   never outlives an edit.
//! - `install_tier` sets `Narrow` only after checking every word.
//!
//! The width check is not the verifier's proof: a `FOR_LOOP` whose A is 253
//! fits a `u8` while its loop touches slot 256, which the verifier grades
//! `Wide`. So the tier is installed from the verifier's verdict by the
//! producer that ran it, on the words it verified with nothing in between
//! (compile finalization and snapshot materialization), and the width check
//! is the independent second condition, never a substitute for the verdict.

use std::ops::{Deref, DerefMut};

use super::{Instr, NarrowInstr};
use crate::compiler::RegisterTier;

/// `Default` is empty `Wide` code (`RegisterTier`'s default).
#[derive(Clone, Debug, Default)]
pub(crate) struct Code {
    words: Box<[Instr]>,
    tier: RegisterTier,
}

/// Equal when the canonical words are. The tier is derived from them by a
/// producer, so it is left out: a mutable borrow that edits nothing drops it
/// without changing the instructions.
impl PartialEq for Code {
    fn eq(&self, other: &Self) -> bool {
        self.words == other.words
    }
}

impl Code {
    /// The register tier dispatch may rely on.
    pub(crate) fn tier(&self) -> RegisterTier {
        self.tier
    }

    /// Install the tier the verifier selected for exactly these words. A
    /// `Narrow` verdict also requires every word to narrow. A word that does
    /// not means the verifier and the opcode table's register uses disagree,
    /// a VM bug: debug builds stop on it, and release builds keep the
    /// function `Wide`. That prevents truncation aliasing; that an operand
    /// names a live frame slot is still the verifier's proof, under either
    /// tier.
    pub(crate) fn install_tier(&mut self, verdict: RegisterTier) {
        let narrow = verdict == RegisterTier::Narrow
            && self
                .words
                .iter()
                .all(|inst| NarrowInstr::new(*inst).is_some());
        debug_assert!(
            narrow || verdict == RegisterTier::Wide,
            "a verifier-narrow function has a word that does not narrow"
        );
        self.tier = if narrow {
            RegisterTier::Narrow
        } else {
            RegisterTier::Wide
        };
    }

    /// The narrow view of a `Narrow` function's words; `None` for `Wide`.
    /// The shared borrow keeps the words unchanged for as long as the view
    /// lives.
    pub(crate) fn narrow_view(&self) -> Option<NarrowCode<'_>> {
        (self.tier() == RegisterTier::Narrow).then_some(NarrowCode(&self.words))
    }
}

impl From<Vec<Instr>> for Code {
    fn from(words: Vec<Instr>) -> Self {
        Self {
            words: words.into_boxed_slice(),
            tier: RegisterTier::Wide,
        }
    }
}

impl Deref for Code {
    type Target = [Instr];

    fn deref(&self) -> &[Instr] {
        &self.words
    }
}

impl DerefMut for Code {
    /// Any mutable access drops the narrow claim.
    fn deref_mut(&mut self) -> &mut [Instr] {
        self.tier = RegisterTier::Wide;
        &mut self.words
    }
}

/// A `Narrow` function's words, borrowed from its `Code`: every word was
/// checked by `NarrowInstr::new` when the tier was installed, and `Code`
/// drops the tier on any edit, so each fetched word is a narrow word.
#[derive(Clone, Copy)]
pub(crate) struct NarrowCode<'a>(&'a [Instr]);

impl NarrowCode<'_> {
    /// The narrow word at `pc`. Panics past the end, as a slice index does.
    #[inline(always)]
    pub(crate) fn fetch(self, pc: usize) -> NarrowInstr {
        // Checked by `Code::install_tier`, unchanged since: see the module
        // comment.
        NarrowInstr(self.0[pc])
    }

    /// The narrow word at `pc`, if the instruction exists.
    #[inline(always)]
    pub(crate) fn get(self, pc: usize) -> Option<NarrowInstr> {
        self.0.get(pc).copied().map(NarrowInstr)
    }
}
