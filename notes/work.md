# work.md

## Target

Correct the stale return-count encoding comment above `unpack_values` in
`src/lua_std.rs`.

## Plan and implementation

- Verified `RetCount::All` encodes as `OPEN_COUNT` (`u16::MAX`), while
  `RetCount::Fixed(255)` encodes as 255.
- Corrected the comment to distinguish the callback's `MAX_RESULTS` ceiling
  from the narrower `MAX_FIXED_RESULTS` cap for fixed call results.
- Removed the resolved finding from `notes/bugs.md`.
- Runtime behavior, cost accounting and replay-visible results are unchanged.

No implementation residue or open decisions remain. Run `brokkr fmt` and the
full `brokkr check` after all edits, then commit the item on `main` if green.
