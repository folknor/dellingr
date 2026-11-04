"""Fixture tests for the interrupt safepoint loop guard."""

import sys
import unittest

import safepoint_loops as guard
from test_native_calls import SourceFixture


class SafepointLoopTests(SourceFixture):
    def problems(self, text, name="src/lua_std/fixture.rs"):
        return guard.check_sources([self.source(text, name)])

    def test_every_loop_kind_needs_a_decision(self):
        problems = self.problems('''fn f(state: &mut State) {
 for i in 0..n { work(i); }
 while more() { step(); }
 while let Some(x) = next() { use_it(x); }
 loop { if done() { break; } }
}''')
        self.assertEqual(len(problems), 4)
        for line, problem in zip((2, 3, 4, 5), problems):
            self.assertIn(f"fixture.rs:{line}:", problem)

    def test_a_poll_in_the_body_or_a_comment_above_decides(self):
        self.assertEqual(self.problems('''fn f(state: &mut State) {
 for i in 0..n { state.poll_interrupt()?; work(i); }
 // bounded: one pass over the argument string's bytes.
 while more() { step(); }
 // A longer note,
 // safepoint: the callee polls per step.
 loop { if done() { break; } }
 // bounded: nested loops each decide.
 for a in xs {
  // bounded: fixed inner count.
  for b in 0..4 { pair(a, b); }
 }
}'''), [])

    def test_comments_and_literals_cannot_supply_a_poll(self):
        problems = self.problems('''fn f(state: &mut State) {
 for i in 0..n { /* state.poll_interrupt() */ work(i); }
 for i in 0..n { let s = "poll_interrupt()"; work(s); }
 // bounded: a reason that is not directly above the loop.
 let x = 1;
 for i in 0..n { work(i); }
 // bounded:
 for i in 0..n { work(i); }
}''')
        self.assertEqual(len(problems), 4)

    def test_non_loop_for_and_test_code_and_other_files_are_ignored(self):
        self.assertEqual(self.problems('''impl Display for Thing {}
fn g<F>(f: F) where F: for<'a> Fn(&'a u8) {}
#[cfg(test)]
mod tests { fn t() { for i in 0..9 { check(i); } } }
'''), [])
        self.assertEqual(self.problems(
            "fn f() { for i in 0..n { work(i); } }", "src/vm/eval.rs"), [])
        for scoped in ("src/patterns/luapat.rs", "src/vm/table_ops.rs"):
            self.assertEqual(len(self.problems(
                "fn f() { for i in 0..n { work(i); } }", scoped)), 1, scoped)


def main():
    suite = unittest.defaultTestLoader.loadTestsFromTestCase(SafepointLoopTests)
    result = unittest.TextTestRunner(stream=sys.stderr).run(suite)
    if not result.wasSuccessful():
        return 1
    print("ok")
    return 0


if __name__ == "__main__":
    sys.exit(main())
