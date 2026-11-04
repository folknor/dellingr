"""Fixture tests for the public State poison-contract guard."""

import sys
import unittest

import poison_contract as guard
from test_native_calls import SourceFixture


def check(sources, **review):
    """The checker over fixture sources, which define none of the real
    registrations NON_EXECUTING reviews."""
    review.setdefault("non_executing", set())
    return guard.check_sources(sources, **review)


class PoisonContractTests(SourceFixture):
    def problems(self, text):
        return check([self.source(text)])

    def test_new_method_and_late_or_discarded_checks_fail(self):
        problems = self.problems('''impl State {
 pub fn missing(&mut self) -> Result<()> { Ok(()) }
 pub fn late(&mut self) -> Result<()> { self.work(); self.check_usable()?; Ok(()) }
 pub fn discarded(&self) -> Result<()> { self.check_usable(); Ok(()) }
 pub fn nested(&self) -> Result<()> {
  let f = || { self.check_usable()?; Ok(()) }; f()
 }
}''')
        self.assertEqual(len(problems), 4)
        for name, problem in zip(("missing", "late", "discarded", "nested"), problems):
            self.assertIn(f"State::{name}: must refuse", problem)

    def test_lexer_comments_and_literals_cannot_supply_check(self):
        problems = self.problems('''impl State {
 pub fn good(&self) -> Result<()> {
  /* outer /* nested */ comment */ self.check_usable()?; Ok(())
 }
 pub fn bad(&self) -> Result<()> {
  // self.check_usable()?;
  let text = r#"self.check_usable()?; { }"#; Ok(())
 }
}''')
        self.assertEqual(len(problems), 1)
        self.assertIn("State::bad", problems[0])

    def test_visibility_owner_constructors_and_test_code(self):
        self.assertEqual(self.problems('''impl State {
 pub fn checked(&self) -> crate::Result<()> { self.check_usable()?; Ok(()) }
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
  self.check_usable()?; f(self)
 }
}''', "src/vm.rs")
        self.assertEqual(check([public, helper]), [])
        self.assertEqual(len(check([public])), 1)
        unchecked = self.source('''impl State {
 fn guarded<R>(&mut self, f: impl FnOnce(&mut Self) -> Result<R>) -> Result<R> { f(self) }
}''', "src/vm.rs")
        self.assertEqual(len(check([public, unchecked])), 1)

    def test_snapshot_refusal_and_feature_methods_are_checked(self):
        self.assertEqual(self.problems('''impl State {
 #[cfg(feature = "snapshot")]
 pub fn save(&self) -> std::result::Result<(), SaveError> {
  self.check_save_usable()?; Ok(())
 }
}'''), [])
        # A bare poison test misses the floating-point environment.
        problems = self.problems('''impl State {
 #[cfg(feature = "snapshot")]
 pub fn save(&self) -> Result<(), SaveError> {
  if self.poisoned { return Err(SaveError::StatePoisoned); } Ok(())
 }
}''')
        self.assertEqual(len(problems), 1)

    def boundary(self):
        """The guard and an execution boundary, as src/vm.rs defines them."""
        return self.source('''impl State {
 pub(crate) fn guarded<R>(&mut self, f: impl FnOnce(&mut Self) -> Result<R>) -> Result<R> {
  self.check_usable()?; f(self)
 }
 pub(crate) fn call_unguarded(&mut self) -> Result<()> { Ok(()) }
}''', "src/vm.rs")

    def test_lua_reaching_method_needs_whole_body_guard(self):
        methods = self.source('''impl State {
 pub fn good(&mut self) -> Result<()> {
  self.guarded(|state| crate::native_call!("good->call_unguarded", state.call_unguarded()))
 }
 pub fn checked_only(&mut self) -> Result<()> {
  self.check_usable()?;
  crate::native_call!("checked_only->call_unguarded", self.call_unguarded())
 }
 pub fn then_more(&mut self) -> Result<()> {
  self.guarded(|state| Ok(()))?;
  crate::native_call!("then_more->call_unguarded", self.call_unguarded())
 }
 pub fn through_helper(&mut self) -> Result<()> { self.check_usable()?; self.helper() }
 fn helper(&mut self) -> Result<()> {
  crate::native_call!("helper->call_unguarded", self.call_unguarded())
 }
}''')
        problems = check([methods, self.boundary()])
        self.assertEqual(len(problems), 4, problems)
        for name, problem in zip(("checked_only", "then_more", "through_helper"), problems):
            self.assertIn(f"State::{name}: can run Lua", problem)
        # A guard that is not the whole body is also a misplaced guard.
        self.assertIn("lib.rs:10: guarded is only the whole body", problems[3])

    def test_host_code_entries_need_whole_body_guard(self):
        methods = self.source('''pub type RustFunc = fn(&mut State) -> Result<u8>;
type Step = RustFunc;
impl State {
 pub fn scoped<F>(&mut self, f: F) -> Result<()>
 where
  F: FnOnce(&mut Self),
 { self.check_usable()?; f(self); Ok(()) }
 pub fn step(&mut self, f: Step) -> Result<u8> { self.check_usable()?; f(self) }
 pub fn guarded_scope<R>(&mut self, f: impl FnOnce(&mut State) -> R) -> Result<R> {
  self.guarded(|state| Ok(f(state)))
 }
 pub fn add(&mut self, f: RustFunc) -> Result<()> { self.check_usable()?; Ok(()) }
 pub fn plain(&mut self, f: impl FnOnce() -> u8) -> Result<u8> { self.check_usable()?; Ok(f()) }
}''')
        problems = check([methods, self.boundary()], non_executing={"add"})
        self.assertEqual(len(problems), 2, problems)
        for name, problem in zip(("scoped", "step"), problems):
            self.assertIn(f"State::{name}: runs host code", problem)
        stale = check([methods, self.boundary()],
                                    non_executing={"add", "plain"})
        self.assertIn("plain takes no State-executing code", stale[-1])

    def test_guard_is_only_the_whole_body_of_a_public_method(self):
        methods = self.source('''impl State {
 fn index_call(&mut self) -> Result<()> { self.guarded(|state| Ok(())) }
 pub fn nested(&mut self) -> Result<()> {
  self.guarded(|state| state.guarded(|inner| Ok(())))
 }
 pub fn spelled(&mut self) -> Result<()> { self.check_usable()?; State::guarded(self, |s| Ok(())) }
 pub fn valued(&mut self) -> Result<()> { self.check_usable()?; let g = Self::guarded; Ok(()) }
}''')
        problems = check([methods, self.boundary()])
        self.assertEqual(len(problems), 4, problems)
        self.assertTrue(all("guarded" in problem for problem in problems))

    def test_only_host_code_calls_a_guarded_entry(self):
        entry = self.source('''impl State {
 pub fn call(&mut self) -> Result<()> {
  self.guarded(|state| crate::native_call!("call->call_unguarded", state.call_unguarded()))
 }
}''')
        caller = 'fn run(state: &mut State) -> Result<()> { state.call() }'
        library = self.source(caller, "src/lua_std/base.rs")
        problems = check([entry, self.boundary(), library])
        self.assertEqual(len(problems), 1, problems)
        self.assertIn("run calls the guarded entry State::call", problems[0])
        cli = self.source(caller, "src/main.rs")
        self.assertEqual(check([entry, self.boundary(), cli]), [])

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
        # `eager` fails both rules: no checked entry, and a guard that is
        # not the `|state|` closure form.
        self.assertEqual(len(problems), 4, problems)
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
