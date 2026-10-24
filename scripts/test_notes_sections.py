"""Tests for fence-aware section handling in the notes maintenance scripts.

A `## ` line inside a fenced code block is example text, not a section
heading, so notes_drop.py and notes_move.py must not split a section there.
Prints `ok` on success; run as a brokkr script check.
"""
import contextlib
import io
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from notes_drop import drop_sections, main as drop_main
from notes_move import main as move_main, split_section


def scratch_dir():
    """A fresh directory under the repository's gitignored `.scratch/`."""
    scratch = Path(__file__).resolve().parents[1] / ".scratch"
    scratch.mkdir(exist_ok=True)
    return tempfile.TemporaryDirectory(dir=scratch)


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


class NotesMainTests(unittest.TestCase):
    def test_move_rejects_destination_as_source_without_writing(self):
        with scratch_dir() as temp_dir:
            shared = Path(temp_dir) / "shared.md"
            original = b"<<AA-101>>\n## AA-101 - section\nbody\n"
            shared.write_bytes(original)

            with patch.object(sys, "argv", ["notes_move.py", str(shared), str(shared)]):
                self.assertEqual(move_main(), 1)

            self.assertEqual(shared.read_bytes(), original)

    def test_move_leaves_untouched_source_bytes_alone(self):
        with scratch_dir() as temp_dir:
            root = Path(temp_dir)
            dest = root / "destination.md"
            source = root / "source.md"
            untouched = root / "untouched.md"
            dest.write_bytes(b"<<AA-101>>\n")
            source.write_bytes(b"## AA-101 - moved\nbody\n")
            untouched_bytes = b"# untouched\r\n\r\n"
            untouched.write_bytes(untouched_bytes)

            with patch.object(
                sys, "argv", ["notes_move.py", str(dest), str(source), str(untouched)]
            ):
                self.assertEqual(move_main(), 0)

            self.assertIn(b"## AA-101 - moved", dest.read_bytes())
            self.assertNotIn(b"## AA-101 - moved", source.read_bytes())
            self.assertEqual(untouched.read_bytes(), untouched_bytes)

    def test_drop_without_arguments_prints_usage(self):
        stderr = io.StringIO()
        with patch.object(sys, "argv", ["notes_drop.py"]), contextlib.redirect_stderr(stderr):
            result = drop_main()

        self.assertEqual(result, 2)
        self.assertIn("usage:", stderr.getvalue())


def main() -> int:
    loader = unittest.defaultTestLoader
    suite = unittest.TestSuite(
        [
            loader.loadTestsFromTestCase(NotesFenceTests),
            loader.loadTestsFromTestCase(NotesMainTests),
        ]
    )
    result = unittest.TextTestRunner(stream=sys.stderr, verbosity=1).run(suite)
    if not result.wasSuccessful():
        return 1
    print("ok")
    return 0


if __name__ == "__main__":
    sys.exit(main())
