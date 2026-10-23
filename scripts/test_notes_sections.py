"""Tests for fence-aware section handling in the notes maintenance scripts.

A `## ` line inside a fenced code block is example text, not a section
heading, so notes_drop.py and notes_move.py must not split a section there.
Prints `ok` on success; run as a brokkr script check.
"""
import sys
import unittest

from notes_drop import drop_sections
from notes_move import split_section


class NotesFenceTests(unittest.TestCase):
    def test_drop_keeps_heading_text_inside_backtick_fence(self):
        lines = [
            "## AA-101 - first",
            "```lua",
            "## example heading",
            "```",
            "first body",
            "## AA-102 - remove this section",
            "second body",
            "## AA-103 - final",
        ]

        output, found = drop_sections(lines, {"AA-102"})

        self.assertEqual(found, {"AA-102"})
        self.assertIn("## example heading", output)
        self.assertNotIn("## AA-102 - remove this section", output)
        self.assertIn("## AA-103 - final", output)

    def test_move_stops_after_fenced_tilde_block(self):
        lines = [
            "## AA-201 - first",
            "~~~markdown",
            "## example heading",
            "~~~~",
            "first body",
            "## AA-202 - second",
            "second body",
        ]

        body_end, end = split_section(lines, 0)

        self.assertEqual(end, 5)
        self.assertEqual(lines[0:body_end], lines[0:5])


def main() -> int:
    suite = unittest.defaultTestLoader.loadTestsFromTestCase(NotesFenceTests)
    result = unittest.TextTestRunner(stream=sys.stderr, verbosity=1).run(suite)
    if not result.wasSuccessful():
        return 1
    print("ok")
    return 0


if __name__ == "__main__":
    sys.exit(main())
