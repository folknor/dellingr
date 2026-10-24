"""Tests for scripts/example_list.py, the example corpus list's checker.

Each test builds a small tree (an examples/ directory and a list file) under
the repository's gitignored `.scratch/` and runs the checker's entry point
on it. Prints `ok` on success; run as a brokkr script check.
"""
import contextlib
import io
import os
import sys
import tempfile
import unittest
from pathlib import Path

import example_list


def scratch_dir():
    """A fresh directory under the repository's gitignored `.scratch/`."""
    scratch = Path(__file__).resolve().parents[1] / ".scratch"
    scratch.mkdir(exist_ok=True)
    return tempfile.TemporaryDirectory(dir=scratch)


def run(root, *arguments):
    """The checker's exit code and stdout for `arguments` on `root`."""
    stdout = io.StringIO()
    with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(io.StringIO()):
        code = example_list.main(list(arguments), root=root)
    return code, stdout.getvalue()


def tree(root, *paths):
    """Creates each root-relative file in `paths` and the list's directory."""
    for path in paths:
        file = root / path
        file.parent.mkdir(parents=True, exist_ok=True)
        file.write_text("print('x')\n", encoding="utf-8")
    (root / example_list.LIST).parent.mkdir(parents=True, exist_ok=True)


class ExampleListTests(unittest.TestCase):
    def test_written_list_checks_and_prints_its_paths(self):
        with scratch_dir() as temp:
            root = Path(temp)
            tree(root, "examples/b.lua", "examples/a/c.lua", "examples/a.lua", "examples/notes.md")
            self.assertEqual(run(root, "--write")[0], 0)
            self.assertEqual(run(root), (0, "ok\n"))
            text = (root / example_list.LIST).read_text(encoding="utf-8")
            self.assertIn('    a_c: "examples/a/c.lua",\n', text)
            # Byte order: "a.lua" sorts before "a/c.lua" ('.' < '/').
            self.assertEqual(
                run(root, "--paths"),
                (0, "examples/a.lua\nexamples/a/c.lua\nexamples/b.lua\n"),
            )

    def test_an_unlisted_file_fails_until_written(self):
        with scratch_dir() as temp:
            root = Path(temp)
            tree(root, "examples/a.lua")
            run(root, "--write")
            tree(root, "examples/scratch.lua")
            code, output = run(root)
            self.assertEqual(code, 1)
            self.assertIn("not listed: examples/scratch.lua", output)
            run(root, "--write")
            self.assertEqual(run(root), (0, "ok\n"))

    def test_a_removed_file_fails(self):
        with scratch_dir() as temp:
            root = Path(temp)
            tree(root, "examples/a.lua", "examples/b.lua")
            run(root, "--write")
            (root / "examples/b.lua").unlink()
            code, output = run(root)
            self.assertEqual(code, 1)
            self.assertIn("listed but not on disk: examples/b.lua", output)

    def test_a_hand_edit_outside_the_generated_form_fails(self):
        with scratch_dir() as temp:
            root = Path(temp)
            tree(root, "examples/a.lua")
            run(root, "--write")
            list_file = root / example_list.LIST
            text = list_file.read_text(encoding="utf-8")
            for edited in [
                text.replace("examples! {\n", "examples! {\n    // a comment\n"),
                text.replace('    a: "examples/a.lua",\n', '    b: "examples/a.lua",\n'),
                text + "fn extra() {}\n",
            ]:
                list_file.write_text(edited, encoding="utf-8")
                code, output = run(root)
                self.assertEqual(code, 1, edited)
                self.assertIn("not in the generated form", output)
                self.assertEqual(run(root, "--paths")[0], 1)

    def test_names_must_be_unique_identifiers(self):
        cases = {
            "examples/a/b.lua examples/a_b.lua": "both map to the test name a_b",
            "examples/Upper.lua": "must match [a-z0-9_]+",
            "examples/with-dash.lua": "must match [a-z0-9_]+",
            "examples/1st.lua": "starts with a digit",
            "examples/loop.lua": "is a Rust keyword",
            "examples/_.lua": "cannot name a test",
        }
        for paths, message in cases.items():
            with self.subTest(paths), scratch_dir() as temp:
                root = Path(temp)
                tree(root, *paths.split())
                code, output = run(root, "--write")
                self.assertEqual(code, 1)
                self.assertIn(message, output)
                self.assertFalse((root / example_list.LIST).exists())

    def test_an_empty_corpus_fails(self):
        with scratch_dir() as temp:
            root = Path(temp)
            tree(root)
            (root / "examples").mkdir()
            code, output = run(root, "--write")
            self.assertEqual(code, 1)
            self.assertIn("empty", output)

    @unittest.skipUnless(hasattr(os, "symlink"), "needs symlinks")
    def test_a_symlink_under_examples_fails(self):
        for target in ["file", "directory"]:
            with self.subTest(target), scratch_dir() as temp:
                root = Path(temp)
                tree(root, "examples/a.lua", "elsewhere/b.lua")
                run(root, "--write")
                source = root / ("elsewhere/b.lua" if target == "file" else "elsewhere")
                (root / "examples/link").symlink_to(source)
                code, output = run(root)
                self.assertEqual(code, 1)
                self.assertIn("examples/link is a symlink", output)

    @unittest.skipUnless(hasattr(os, "mkfifo"), "needs FIFOs")
    def test_a_lua_entry_that_is_not_a_regular_file_fails(self):
        with scratch_dir() as temp:
            root = Path(temp)
            tree(root, "examples/a.lua")
            run(root, "--write")
            os.mkfifo(root / "examples/pipe.lua")
            code, output = run(root)
            self.assertEqual(code, 1)
            self.assertIn("examples/pipe.lua is not a regular file", output)

    def test_unknown_arguments_print_usage(self):
        with scratch_dir() as temp:
            self.assertEqual(run(Path(temp), "--check")[0], 2)


def main() -> int:
    suite = unittest.defaultTestLoader.loadTestsFromTestCase(ExampleListTests)
    result = unittest.TextTestRunner(stream=sys.stderr, verbosity=1).run(suite)
    if not result.wasSuccessful():
        return 1
    print("ok")
    return 0


if __name__ == "__main__":
    sys.exit(main())
