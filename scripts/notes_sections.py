"""Markdown section-heading helpers for the notes maintenance scripts."""
import re


_FENCE_START = re.compile(r" {0,3}(`{3,}|~{3,})(.*)")


def section_heading_flags(lines):
    """Return whether each line is a section heading outside fenced code."""
    flags = []
    fence = None
    for line in lines:
        markdown_line = line[:-1] if line.endswith("\r") else line
        if fence is not None:
            marker, length = fence
            closing = re.fullmatch(
                rf" {{0,3}}{re.escape(marker)}{{{length},}}[ \t]*", markdown_line
            )
            if closing:
                fence = None
            flags.append(False)
            continue

        opening = _FENCE_START.fullmatch(markdown_line)
        if opening is not None:
            marker = opening.group(1)
            info = opening.group(2)
            if marker[0] != "`" or "`" not in info:
                fence = (marker[0], len(marker))
                flags.append(False)
                continue

        flags.append(markdown_line.startswith("## "))
    return flags
