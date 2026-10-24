"""Fixture tests for the lexical native-call inventory guard."""
import re
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import native_calls as guard


class SourceFixture(unittest.TestCase):
    """A throwaway source tree under .scratch/, shared by the checkers built
    on native_calls' lexer (test_poison_contract.py subclasses it)."""

    def setUp(self):
        scratch = guard.ROOT / ".scratch"
        scratch.mkdir(exist_ok=True)
        self.directory = tempfile.TemporaryDirectory(dir=scratch)
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        root_patch = patch.object(guard, "ROOT", self.root)
        root_patch.start()
        self.addCleanup(root_patch.stop)

    def source(self, text, name="src/lib.rs"):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)
        return guard.Source(path)


class NativeCallTests(SourceFixture):
    def test_observable_wrapper_owns_one_call(self):
        source = self.source('''impl State {
 fn outer(&mut self) {
  crate::native_call!("outer->call_unguarded",
   self.call_unguarded());
 }
}''')
        _, sites, _ = guard.closure([source])
        problems = []
        guard.attach_markers([source], sites, problems)
        self.assertEqual(problems, [])
        self.assertEqual(sites[0].ids, ["outer->call_unguarded"])

    def test_observable_wrapper_rejects_bad_id_wrong_call_and_multiple_calls(self):
        for expression, expected in [
            ('"broken", self.call_unguarded()', "malformed native-call wrapper"),
            ('"other->call_unguarded", self.call_unguarded()', "does not match its call"),
            ('"outer->call_unguarded", (self.call_unguarded(), self.call_unguarded())',
             "exactly one inventoried call"),
            ('"outer->call_unguarded", 7', "exactly one inventoried call"),
        ]:
            with self.subTest(expression=expression):
                source = self.source("impl State { fn outer(&mut self) { "
                                     + "crate::native_call!(" + expression + "); } }")
                _, sites, _ = guard.closure([source])
                problems = []
                guard.attach_markers([source], sites, problems)
                self.assertTrue(any(expected in problem for problem in problems), problems)

    def test_wrappers_in_literals_comments_and_test_items_are_inactive(self):
        source = self.source('''impl State {
 fn outer(&mut self) {
  let text = r#"native_call!("outer->call_unguarded", self.call_unguarded())"#;
  // native_call!("outer->call_unguarded", self.call_unguarded())
 }
 #[cfg(test)] fn ignored(&mut self) {
  crate::native_call!("ignored->call_unguarded", self.call_unguarded());
 }
}''')
        _, sites, _ = guard.closure([source])
        problems = []
        guard.attach_markers([source], sites, problems)
        self.assertEqual(problems, [])
        self.assertEqual(sites, [])

    def test_textlint_requires_the_observable_marker(self):
        names = {"call_unguarded"}
        rule = {
            "name": guard.TEXTLINT_RULE,
            "pattern": guard.textlint_pattern(names),
            "except": [r"\bfn\s+(?:call_unguarded)\b"],
            "allow_marker": "native_call!",
            "allow_marker_above": 2,
        }
        values = {"name": guard.METHOD_VALUE_RULE,
                  "pattern": guard.method_value_pattern(names)}
        config = {"textlint": [rule, values]}
        problems = []
        guard.check_textlint(names, problems, config)
        self.assertEqual(problems, [])
        # A one-line window misses rustfmt's vertical wrapper layout.
        rule["allow_marker_above"] = 1
        guard.check_textlint(names, problems, config)
        self.assertTrue(any("allow_marker_above = 2" in problem for problem in problems))
        problems.clear()
        rule["allow_marker_above"] = 2
        rule["allow_marker"] = "comment"
        guard.check_textlint(names, problems, config)
        self.assertTrue(any("allow_marker" in problem for problem in problems))

    def test_method_values_are_distinct_from_calls_and_callback_registration(self):
        pattern = re.compile(guard.method_value_pattern({"call_unguarded", "format"}))
        for source in ["let f = State::call_unguarded;",
                       "take(Self::call_unguarded)",
                       "[Frame::call_unguarded, other]",
                       "State::call_unguarded as Callback",
                       "take(State::call_unguarded::<T>)"]:
            with self.subTest(source=source):
                self.assertIsNotNone(pattern.search(source))
        for source in ["State::call_unguarded(state)",
                       "Self::call_unguarded::<T>(state)",
                       "fn call_unguarded() {}",
                       '("format", string_format::format)',
                       "register(format)"]:
            with self.subTest(source=source):
                self.assertIsNone(pattern.search(source))

    def test_lexer_blanks_literals_nested_comments_but_keeps_offsets(self):
        text = '''fn live<'a>() {
 let a = br##"call_unguarded() { // hidden }"##;
 let b = "escaped \\" call_unguarded()";
 let c = '\\''; let d = b'x';
 /* outer /* call_unguarded() */ still hidden */
 state.call_unguarded(); // ordinary line comment
}'''
        source = self.source(text)
        self.assertEqual(len(source.code), len(text))
        self.assertEqual(source.code.count("\n"), text.count("\n"))
        self.assertEqual([name for name, *_ in source.calls()], ["call_unguarded"])
        self.assertEqual(source.comments[6], "// ordinary line comment")

    def test_receivers_nested_functions_and_test_items(self):
        source = self.source('''impl State {
 fn outer(&mut self) {
  self.call_unguarded();
  fn inner() { State::call_unguarded(); }
  Self::call_unguarded();
  state.call_unguarded(); other.call_unguarded();
  Other::call_unguarded();
 }
 #[cfg(test)] fn ignored() { state.call_unguarded(); }
}''')
        reached, sites, _ = guard.closure([source])
        self.assertIn(("State", "inner"), reached)
        self.assertEqual([site.caller.name for site in sites],
                         ["outer", "inner", "outer", "outer", "outer"])
        for offset in range(len(source.code)):
            expected = [f for f in source.fns if f.body_open < offset < f.body_close]
            expected = max(expected, key=lambda f: f.body_open) if expected else None
            self.assertIs(source.enclosing(offset), expected)
            self.assertEqual(source.in_test(offset),
                             any(a <= offset < b for a, b in source.tests))

    def test_callback_closure_and_markers(self):
        source = self.source('''fn register() {
 let callbacks = [("entry", |state| {
 crate::native_call!("lib:entry->call_unguarded",
  state.call_unguarded());
 })];
}''', "src/basic.rs")
        _, sites, _ = guard.closure([source])
        problems = []
        guard.attach_markers([source], sites, problems)
        self.assertEqual(problems, [])
        self.assertEqual(sites[0].ids, ["lib:entry->call_unguarded"])

    def test_legacy_native_call_comment_is_rejected(self):
        self.source("#[cfg(test)] mod test_only;\n", "src/lib.rs")
        self.source("// native-call: lib:entry->call_unguarded\n", "src/test_only.rs")
        with patch.object(guard, "SRC", self.root / "src"):
            all_sources = guard.source_files()
            production = guard.production_sources(all_sources)
        self.assertNotIn("src/test_only.rs", [source.rel for source in production])
        problems = []
        guard.reject_legacy_markers(all_sources, problems)
        self.assertEqual(problems, [
            "src/test_only.rs:1: legacy native-call comment is unsupported; "
            "use a native_call! wrapper"
        ])

    def test_lists_ignore_comments_strings_nested_fields_and_count_native(self):
        source = self.source('''const SCENARIOS: &[Scenario] = &[
 // Scenario { name: "comment", native: 99 },
 Scenario { name: "live", native: 2, step: r#"name: "fake""#,
 nested: Other { name: "nested", native: 99 } },
];
const CHAINS: &[Chain] = &[Chain { name: "chain" }];
const LEAVES: &[Leaf] = &[Leaf { name: "leaf" }];''')
        problems = []
        counts = {}
        with patch.object(guard, "TEST", source.path):
            lists = guard.scan_test_lists(problems, counts)
        self.assertEqual(problems, [])
        self.assertEqual(lists, {"SCENARIOS": ["live"], "CHAINS": ["chain"],
                                 "LEAVES": ["leaf"]})
        self.assertEqual(counts, {"live": 2})
        path = ["call_unguarded->invoke_rust_fn", "handler->call_unguarded",
                "call_unguarded->run", "run->call_unguarded"]
        guard.check_native_count("live", path, counts["live"], problems)
        self.assertEqual(problems, [])
        guard.check_native_count("live", path, 1, problems)
        self.assertIn("does not match", problems[0])

    def test_closure_preserves_full_scan_results_and_stray_diagnostics(self):
        source = self.source('''impl State {
 fn middle(&mut self) { self.call_unguarded(); }
 fn outer(&mut self) { self.middle(); }
}
fn unrelated() { Other::middle(); }
''')
        calls = [(source, *call) for call in source.calls()]
        reached = {key: None for key in guard.BOUNDARIES}
        queue = list(guard.BOUNDARIES)
        expected = []
        while queue:
            key = queue.pop()
            for src, name, offset, caller, owner in calls:
                if not guard.resolves(key, name, owner) or caller is None:
                    continue
                expected.append((src.rel, offset, caller.key, key))
                if caller.key not in reached:
                    reached[caller.key] = caller
                    queue.append(caller.key)
        actual_reached, sites, toplevel = guard.closure([source])
        self.assertEqual(set(actual_reached), set(reached))
        self.assertEqual([(s.src.rel, s.offset, s.caller.key, s.callee) for s in sites],
                         sorted(expected, key=lambda item: item[:2]))
        self.assertEqual(toplevel, [])
        self.assertEqual(guard.textlint_strays([source], sites, {"middle"}),
                         ["src/lib.rs:5: fn unrelated() { Other::middle(); }"])

    def test_cfg_test_out_of_line_modules_and_hotpath_attributes(self):
        self.source('''#[cfg(test)] mod helpers;
mod production;
''')
        self.source('fn hidden() { state.call_unguarded(); }', "src/helpers.rs")
        live = self.source('''#[cfg_attr(feature = "hotpath", hotpath::measure)]
fn live() { state.call_unguarded(); }
''', "src/production.rs")
        with patch.object(guard, "SRC", self.root / "src"):
            sources = guard.production_sources()
        self.assertEqual([s.rel for s in sources], ["src/lib.rs", "src/production.rs"])
        self.assertIn("hotpath::measure", guard.item_attributes(live.fns[0]))
        self.assertTrue(guard.has_hotpath_measure(live.fns[0]))

    def test_hotpath_measure_attribute_spellings(self):
        for attribute in (
                '#[hotpath::measure]',
                '#[hotpath::measure(label = "work")]',
                '#[cfg_attr(feature = "hotpath", hotpath::measure)]',
                '#[cfg_attr(feature = "hotpath", hotpath::measure(label = "work"))]',
                '#[cfg_attr(feature = "hotpath",\n hotpath :: measure)]'):
            with self.subTest(attribute=attribute):
                source = self.source(attribute + '\nfn live() {}')
                self.assertTrue(guard.has_hotpath_measure(source.fns[0]))
        source = self.source('''#[cfg_attr(feature = "hotpath", inline)]
fn plain() { hotpath::measure(); }
''')
        self.assertFalse(guard.has_hotpath_measure(source.fns[0]))


def main():
    suite = unittest.defaultTestLoader.loadTestsFromTestCase(NativeCallTests)
    result = unittest.TextTestRunner(stream=sys.stderr).run(suite)
    if not result.wasSuccessful():
        return 1
    print("ok")
    return 0


if __name__ == "__main__":
    sys.exit(main())
