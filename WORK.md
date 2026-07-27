# WORK.md

Nothing in flight. Fill this in with the next loop's problem statement
before launching a session.

This file holds only the current loop's problem statement and plan -
sessions read it and should see the problem, not the process. The process
is `notes/fix-loop.md`; loop history lives in git.

---

## Target(s)

<!-- What is broken or slow, the cited code, and the measurement or reading
     that says so. One loop may carry several independent fixes; number them
     and say which are out of scope. -->

### Constraints (inline; sessions read nothing else)

Sessions are pointed at this file and nothing else - never at `CLAUDE.md`
or `AGENTS.md`. Every constraint the session needs must be restated here.

- Read/write code only; no cargo/brokkr/test/bench commands. The
  orchestrator runs `brokkr fmt` and `brokkr check` in the main
  conversation.
- Determinism is a product requirement, not a preference.
- Clippy is strict and denies warnings project-wide: `unwrap_used` denied
  outside `#[cfg(test)]`, `Result::ok()` banned, `HashMap`/`HashSet`
  banned (use `IndexMap` or `BTreeMap`/`BTreeSet`).
- No gremlins: no em-dash, en-dash, or smart quotes anywhere, including
  comments and doc comments.

<!-- Then the loop-specific constraints: what must stay byte-identical,
     which tests are the oracle and may not change, whether `cost_used` is
     allowed to move, which `#[hotpath::measure]` annotations must survive. -->

### Open questions for the reviewer

<!-- Where the independent read and the deep session are most likely to
     disagree. -->

### Deliverable

<!-- What the session hands back. -->

---

## Agreed plan

<!-- Written after the deep session and the independent read converge.
     Implement exactly this. -->

### Bench acceptance

<!-- For a speed loop: which workloads, which phase metric, and what
     counts as accepted. Interleave matched arms; a same-day matched pair
     beats any historical number. -->
