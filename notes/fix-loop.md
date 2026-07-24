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

Notes:

- Steps 4 and 7 are `echo "..." | review bare --profile deep|build`. The
  session ID is printed above the response; keep it for step 8.
- Step 4 runs in the background so step 5 overlaps it. Everything else is
  synchronous.
- Loop history lives in git, not in `WORK.md`.
