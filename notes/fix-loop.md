# Bug-fix loop methodology

The process driving `WORK.md` against `notes/bugs.md`. `WORK.md` itself holds
only the current loop's problem statement and plan - codex sessions read it and
should see the problem, not the process.

One loop:

1. Pick the next target(s) from `notes/bugs.md`.
2. Shallow-verify the target(s) are real by reading the cited code.
3. Write the problem statement into `WORK.md`.
4. Launch `review bare --profile deep` (codex, read-only, xhigh) pointed at
   `WORK.md`: verify the targets independently, produce an implementation plan.
5. While 4 runs: read the target code in depth and produce an independent plan.
6. Consolidate. Argue with the reviewer until the plans agree. Write the agreed
   plan into `WORK.md`.
7. Launch `review bare --profile build` (codex, workspace-write) to implement.
8. Review the diff twice: once directly, once by resuming the deep session from
   step 4 with `--session <ID>`.
9. Actionable findings get fixed - by a build session (findings written into
   `WORK.md`) or by hand.
10. Update `notes/bugs.md`, run `brokkr fmt` + `brokkr check`, commit.

`brokkr check` is the whole gate. Its `diff-lua` script_check runs
`diff_gate.sh` at `stage = "post-test"`, so the differential comparison against
reference Lua 5.2/5.4 is already covered - do not also run `./diff_test.sh`.

Notes:

- Steps 4 and 7 are `echo "..." | review bare --profile deep|build`. The
  session ID is printed above the response; keep it for step 8.
- **Codex refusal fallback.** Codex Sol's safety filter sometimes trips on
  parts of this codebase. It is a false positive - VM internals, GC roots, and
  hostile-input hardening read like exploit work out of context. When it
  refuses or errors out on an item, redo that item with the `Agent` tool, then
  go back to codex for the next item. Do not water down the prompt to get past
  the filter, and do not treat a refusal as a finding about the work.
  - Reviewing / planning (steps 4, 8): `model: "fable"`, which has never
    tripped.
  - Implementing (step 7): `model: "opus"`.
  - Either way the agent writes and reads only. The orchestrator runs
    `brokkr fmt` / `brokkr check` in the main conversation, so parallel agents
    never contend over a build.
- Step 4 runs in the background so step 5 overlaps it. Everything else is
  synchronous.
- **Never mutate the working tree while a review session is running.** A
  `review` session starts fresh and fetches code itself: it runs `git diff` and
  opens files in the live tree. A `git stash`, `git checkout`, or any edit
  during that window silently changes what it reviews, and the verdict that
  comes back cannot be placed. Worktrees are not an escape hatch here - they
  are banned project-wide.
  - Concretely: baseline benchmarking (`git stash` -> `hotbench` -> `git stash
    pop`) must happen *before* launching the reviewer or *after* it returns,
    never alongside it.
  - The overlap in step 5 is read-only work only. Reading is fine; writing is
    not.
- Loop history lives in git, not in `WORK.md`.
