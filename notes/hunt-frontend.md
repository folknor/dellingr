Found one defect.

**[P2] Invalid string escapes become falsely recoverable at EOF** - [lexer.rs](/home/folk/Programs/dellingr/src/compiler/lexer.rs:466).

Compiling `return "\q` with EOF immediately after `q` returns `SyntaxError::UnfinishedString`, making `is_recoverable()` return `true`. The same happens with unfinished strings containing `\256` or `\xG`.

The lexer has already recorded a permanent escape error, but both EOF branches discard it. Appending text cannot repair those escapes. Adding the closing quote merely exposes the original error.

This breaks [`SyntaxError::is_recoverable`'s contract](/home/folk/Programs/dellingr/src/error.rs:1642): appending text can complete the construct. It also fails [compatibility.md's narrower promise](/home/folk/Programs/dellingr/docs/compatibility.md) about completing the last token, independently of preceding grammar. Hosts using this API are incorrectly told to request more input.

Preserve permanent escape failures at EOF while keeping incomplete valid prefixes such as `\x4` recoverable.

No other confirmed defects found in scope. Compiler, globals, and numeral tests passed, plus 2,000 generated hex conversions and a 10,000-byte mixed-escape string probe. The working tree remains unchanged.
