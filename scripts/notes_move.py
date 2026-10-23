#!/usr/bin/env python3
"""Move whole `## <ID> - ...` sections from findings documents into another.

Usage: scripts/notes_move.py <dest> <source> [<source> ...]

<dest> holds placeholder lines of the form `<<ID>>`. Each is replaced by the
section with that ID, cut from whichever source holds it. A section runs from
its `## ` heading to the next `## ` heading outside a fenced code block, or
EOF; trailing blank lines and `---` separators stay in the source. Fails before
writing anything if a placeholder has no section or a wanted ID occurs more
than once among the sources.
"""
import re
import sys

from notes_sections import section_heading_flags


def split_section(lines, start, heading_flags=None):
    if heading_flags is None:
        heading_flags = section_heading_flags(lines)
    end = next(
        (index for index in range(start + 1, len(lines)) if heading_flags[index]),
        len(lines),
    )
    body_end = end
    while body_end > start + 1 and lines[body_end - 1].strip() in ("", "---"):
        body_end -= 1
    return body_end, end


def main() -> int:
    dest, sources = sys.argv[1], sys.argv[2:]
    with open(dest, encoding="utf-8") as f:
        dest_lines = f.read().split("\n")
    wanted = [m.group(1) for line in dest_lines if (m := re.fullmatch(r"<<([A-Z]+-\d{3})>>", line))]
    texts = {}
    for path in sources:
        with open(path, encoding="utf-8") as f:
            texts[path] = f.read().split("\n")
    sections = {}
    duplicates = []
    for path, lines in texts.items():
        i = 0
        heading_flags = section_heading_flags(lines)
        while i < len(lines):
            m = re.match(r"## ([A-Z]+-\d{3}) - ", lines[i]) if heading_flags[i] else None
            if m and m.group(1) in wanted:
                section_id = m.group(1)
                body_end, _end = split_section(lines, i, heading_flags)
                if section_id in sections:
                    if section_id not in duplicates:
                        duplicates.append(section_id)
                else:
                    sections[section_id] = lines[i:body_end]
                del lines[i:body_end]
                del heading_flags[i:body_end]
                while i < len(lines) and lines[i] == "" and i > 0 and lines[i - 1] == "":
                    del lines[i]
                    del heading_flags[i]
                continue
            i += 1
    if duplicates:
        print(f"duplicate sections for: {', '.join(duplicates)}", file=sys.stderr)
        return 1
    missing = [w for w in wanted if w not in sections]
    if missing:
        print(f"no section for: {', '.join(missing)}", file=sys.stderr)
        return 1
    out = []
    for line in dest_lines:
        m = re.fullmatch(r"<<([A-Z]+-\d{3})>>", line)
        out.extend(sections[m.group(1)] if m else [line])
    with open(dest, "w", encoding="utf-8") as f:
        f.write("\n".join(out).rstrip("\n") + "\n")
    for path, lines in texts.items():
        with open(path, "w", encoding="utf-8") as f:
            f.write("\n".join(lines).rstrip("\n") + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
