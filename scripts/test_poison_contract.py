"""Fixture tests for the public State poison-contract guard."""

import sys
import unittest

import poison_contract as guard
from test_native_calls import SourceFixture


class PoisonContractTests(SourceFixture):
    def problems(self, text):
        return guard.check_sources([self.source(text)])

    def test_new_method_and_late_or_discarded_checks_fail(self):
        problems = self.problems('''impl State {
 pub fn missing(&mut self) -> Result<()> { Ok(()) }
 pub fn late(&mut self) -> Result<()> { self.work(); self.check_poisoned()?; Ok(()) }
 pub fn discarded(&self) -> Result<()> { self.check_poisoned(); Ok(()) }
 pub fn nested(&self) -> Result<()> {
  let f = || { self.check_poisoned()?; Ok(()) }; f()
 }
}''')
        self.assertEqual(len(problems), 4)
        for name, problem in zip(("missing", "late", "discarded", "nested"), problems):
            self.assertIn(f"State::{name}: must refuse", problem)

    def test_lexer_comments_and_literals_cannot_supply_check(self):
        problems = self.problems('''impl State {
 pub fn good(&self) -> Result<()> {
  /* outer /* nested */ comment */ self.check_poisoned()?; Ok(())
 }
 pub fn bad(&self) -> Result<()> {
  // self.check_poisoned()?;
  let text = r#"self.check_poisoned()?; { }"#; Ok(())
 }
}''')
        self.assertEqual(len(problems), 1)
        self.assertIn("State::bad", problems[0])

    def test_visibility_owner_constructors_and_test_code(self):
        self.assertEqual(self.problems('''impl State {
 pub fn checked(&self) -> crate::Result<()> { self.check_poisoned()?; Ok(()) }
 pub(crate) fn internal(&self) -> Result<()> { Ok(()) }
 pub(super) fn internal_too(&self) -> Result<()> { Ok(()) }
 pub fn constructor() -> Result<Self> { make() }
 #[cfg(test)] pub fn test_only(&self) -> Result<()> { Ok(()) }
}
impl Other { pub fn unrelated(&self) -> Result<()> { Ok(()) } }
'''), [])

    def test_guarded_delegation_requires_checked_helper_across_files(self):
        public = self.source('''impl State {
 pub fn run(&mut self) -> Result<()> { self.guarded(|state| state.run_inner()) }
}''')
        helper = self.source('''impl State {
 pub(crate) fn guarded<R>(&mut self, f: impl FnOnce(&mut Self) -> Result<R>) -> Result<R> {
  self.check_poisoned()?; f(self)
 }
}''', "src/vm.rs")
        self.assertEqual(guard.check_sources([public, helper]), [])
        self.assertEqual(len(guard.check_sources([public])), 1)
        unchecked = self.source('''impl State {
 fn guarded<R>(&mut self, f: impl FnOnce(&mut Self) -> Result<R>) -> Result<R> { f(self) }
}''', "src/vm.rs")
        self.assertEqual(len(guard.check_sources([public, unchecked])), 1)

    def test_snapshot_refusal_and_feature_methods_are_checked(self):
        self.assertEqual(self.problems('''impl State {
 #[cfg(feature = "snapshot")]
 pub fn save(&self) -> std::result::Result<(), SaveError> {
  if self.poisoned { return Err(save_state::SaveError::StatePoisoned); } Ok(())
 }
}'''), [])
        problems = self.problems('''impl State {
 #[cfg(feature = "snapshot")]
 pub fn save(&self) -> Result<(), SaveError> {
  if self.poisoned { return Err(SaveError::Other); } Ok(())
 }
}''')
        self.assertEqual(len(problems), 1)

    def test_string_delegation_requires_checked_renderer(self):
        wrapper = self.source('''impl State {
 pub fn to_string_with_meta(&mut self, idx: isize) -> Result<String> {
  let raw = self.tostring_bytes(idx)?; Self::lossy_utf8_checked(&raw)
 }
}''')
        renderer = self.source('''impl State {
 pub fn tostring_bytes(&mut self, idx: isize) -> Result<Vec<u8>> {
  self.check_poisoned()?; render(idx)
 }
}''', "src/vm.rs")
        self.assertEqual(guard.check_sources([wrapper, renderer]), [])
        self.assertEqual(len(guard.check_sources([wrapper])), 1)

    def test_infallible_review_does_not_exempt_fallible_methods(self):
        problems = self.problems('''impl State {
 pub fn cost_used(&self) -> u64 { 0 }
 pub fn fresh(&self, f: impl FnOnce() -> Result<()>) -> bool { false }
 pub fn get_top(&self) -> Result<usize> { Ok(0) }
}''')
        self.assertEqual(len(problems), 2)
        self.assertIn("State::fresh: review", problems[0])
        self.assertIn("State::get_top: must refuse", problems[1])

    def test_unknown_helpers_and_no_receivers_fail_closed(self):
        problems = self.problems('''impl State {
 pub fn run(&mut self) -> Result<()> { self.with_unchecked(|_| Ok(())) }
 pub fn consume(self) -> Result<()> { Ok(()) }
 pub fn eager(&mut self) -> Result<()> { self.guarded(self.make_callback()) }
}''')
        self.assertEqual(len(problems), 3)
        self.assertEqual(self.problems("impl Other {}"),
                         ["no public State receiver methods found"])


def main():
    suite = unittest.defaultTestLoader.loadTestsFromTestCase(PoisonContractTests)
    result = unittest.TextTestRunner(stream=sys.stderr).run(suite)
    if not result.wasSuccessful():
        return 1
    print("ok")
    return 0


if __name__ == "__main__":
    sys.exit(main())
