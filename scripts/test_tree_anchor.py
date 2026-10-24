#!/usr/bin/env python3
"""Check, from outside the integration test crate, that its convention tests
are compiled and run.

`tests/integration/corpus/test_conventions.rs` keeps the test tree to its
named targets and every source reachable from `main.rs`, but a `cfg` on its own
`mod` line, an inner `cfg`, an `#[ignore]` or a deleted declaration would
switch it off without any test failing. A `brokkr check` script check runs
this, so that switch-off fails the gate. Prints `ok`.
"""
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
INTEGRATION = ROOT / "tests" / "integration"

# (declaring file, the module declaration it must hold ungated)
DECLARATIONS = [
    (INTEGRATION / "main.rs", "mod corpus;"),
    (INTEGRATION / "corpus" / "mod.rs", "mod test_conventions;"),
]
CHECKER = INTEGRATION / "corpus" / "test_conventions.rs"


CHAR_LITERAL = re.compile(r"'(?:[^'\\\n]|\\[^']{1,10})'")
RAW_STRING = re.compile(r'r(#*)"')


def without_comments(text: str, empty_strings: bool = False) -> str:
    """`text` with every comment (nested block comments included) replaced by
    a space, keeping newlines. String and char literals are kept, or emptied
    with `empty_strings`."""
    out, index = [], 0
    while index < len(text):
        char_literal = CHAR_LITERAL.match(text, index)
        raw = RAW_STRING.match(text, index)
        if raw and (index == 0 or not (text[index - 1].isalnum() or text[index - 1] == "_")):
            close = '"' + raw.group(1)
            end = text.find(close, raw.end())
            end = len(text) if end < 0 else end + len(close)
            literal = text[index:end]
            out.append('""' + "\n" * literal.count("\n") if empty_strings else literal)
            index = end
            continue
        if char_literal:
            out.append("' '" if empty_strings else char_literal.group(0))
            index = char_literal.end()
            continue
        if text.startswith("//", index):
            end = text.find("\n", index)
            index = len(text) if end < 0 else end
            out.append(" ")
        elif text.startswith("/*", index):
            depth = 0
            while index < len(text):
                if text.startswith("/*", index):
                    depth, index = depth + 1, index + 2
                elif text.startswith("*/", index):
                    depth, index = depth - 1, index + 2
                    if depth == 0:
                        break
                else:
                    if text[index] == "\n":
                        out.append("\n")
                    index += 1
            out.append(" ")
        elif text[index] == '"':
            end = index + 1
            while end < len(text) and text[end] != '"':
                end += 2 if text[end] == "\\" else 1
            literal = text[index:end + 1]
            out.append('""' + "\n" * literal.count("\n") if empty_strings else literal)
            index = end + 1
        else:
            out.append(text[index])
            index += 1
    return "".join(out)


def problems() -> list[str]:
    found = []
    for path, declaration in DECLARATIONS:
        relative = path.relative_to(ROOT)
        lines = without_comments(path.read_text(encoding="utf-8")).splitlines()
        if any("#![" in line for line in lines):
            found.append(f"{relative}: an inner attribute can gate the whole module")
        positions = [i for i, line in enumerate(lines) if line.strip() == declaration]
        if len(positions) != 1:
            found.append(f"{relative}: expected one `{declaration}` line")
            continue
        # With comments gone, the code before the declaration must end with
        # another item's `;`, or be empty: an attribute, wrapped or not, fails.
        previous = next(
            (line.strip() for line in reversed(lines[: positions[0]]) if line.strip()), ""
        )
        if previous and not previous.endswith(";"):
            found.append(
                f"{relative}: `{declaration}` must follow another item, with no attribute: "
                f"found `{previous}`"
            )
    checker = without_comments(CHECKER.read_text(encoding="utf-8"), empty_strings=True).splitlines()
    for number, line in enumerate(checker, 1):
        stripped = line.strip()
        if any(marker in stripped for marker in ("#![", "#[cfg", "#[ignore")):
            found.append(f"{CHECKER.relative_to(ROOT)}:{number}: `{stripped}` can switch the checks off")
    if not any(line.strip() == "#[test]" for line in checker):
        found.append(f"{CHECKER.relative_to(ROOT)}: no tests")
    return found


def main() -> int:
    found = problems()
    for problem in found:
        print(problem)
    if found:
        return 1
    print("ok")
    return 0


if __name__ == "__main__":
    sys.exit(main())
