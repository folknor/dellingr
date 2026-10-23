#!/usr/bin/env python3
"""Print the native frame size of every function in a binary whose symbol
matches one of the given substrings.

Usage: scripts/frame_sizes.py [--calls | --body] <binary> <substring>...

Frame size is read from straight-line prologue `sub $N, %rsp` instructions
(x86-64 only), plus pushes, up to the first control transfer. This includes
unrolled stack probes, but cannot measure looped probes or dynamic frames.
A function with no `sub rsp` prints its push count only. Symbols are
demangled by objdump; the match is a plain substring test against the
demangled name, so `call_unguarded` matches every monomorphization and
closure containing it. Functions that were inlined into their callers do not
appear; measure the caller then.

With `--calls`, each matched function is followed by the distinct call
targets in its body, one per line, indented. These help compare inlining;
indirect calls and tail jumps are not included. With `--body`, the
matched functions' instructions are printed with addresses blanked, so two
builds' bodies can be diffed to find where spills or live ranges changed.
"""

import re
import subprocess
import sys


def main() -> int:
    args = sys.argv[1:]
    calls = False
    body_dump = False
    if args and args[0] == "--calls":
        calls = True
        args = args[1:]
    elif args and args[0] == "--body":
        body_dump = True
        args = args[1:]
    if len(args) < 2:
        print(__doc__, file=sys.stderr)
        return 2
    binary = args[0]
    needles = args[1:]
    text = subprocess.run(
        ["objdump", "-d", "--no-show-raw-insn", "-C", binary],
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    header = re.compile(r"^[0-9a-f]+ <(.*)>:$")
    call = re.compile(r"call\s+[0-9a-f]+ <(.*)>$")
    name = None
    matched = False
    pushes = 0
    allocation = 0
    prologue = True
    targets: list[str] = []

    def flush() -> None:
        if matched and not body_dump:
            print(f"{allocation + 8 * pushes}\tsub={allocation}\tpushes={pushes}\t{name}")
        if calls and matched:
            for target in targets:
                print(f"\t{target}")

    for line in text.splitlines():
        m = header.match(line)
        if m:
            flush()
            name = m.group(1)
            matched = any(n in name for n in needles)
            pushes = 0
            allocation = 0
            prologue = True
            targets = []
            if matched and body_dump:
                print(f"<{name}>:")
            continue
        if name is None or not matched:
            continue
        body = line.split("\t", 1)[1].strip() if "\t" in line else ""
        if body_dump:
            # Addresses stripped so two builds diff on instructions alone.
            print(re.sub(r"\b(?=[0-9a-f]*[0-9])[0-9a-f]{4,}\b", "ADDR", body))
            continue
        c = call.search(body)
        if c and c.group(1) not in targets:
            targets.append(c.group(1))
        if not prologue:
            continue
        if body.startswith("push"):
            pushes += 1
            continue
        sub = re.match(r"sub\s+\$0x([0-9a-f]+),%rsp", body)
        if sub:
            allocation += int(sub.group(1), 16)
        elif body.startswith(("call", "ret", "j")):
            prologue = False
    flush()
    return 0


if __name__ == "__main__":
    sys.exit(main())
