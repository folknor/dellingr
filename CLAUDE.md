@AGENTS.md

## Multi-Agent Orchestration

**Do NOT use worktree isolation for parallel agents.** Worktrees create merge conflicts that silently drop agent work. Instead, launch agents in the same tree with strict file ownership - zero overlap.

**Why no worktrees:** Worktrees let agents work on diverged snapshots. When merging back, `git checkout --ours/--theirs` drops code, conflict markers get missed, and features end up "existing but not wired" - types/functions created but never connected to bytecode dispatch, the standard library, or call sites. This has happened in long sessions and was only caught by a rigorous 3-pass audit.

**Agent coordination rules:**

- Each agent gets exclusive ownership of specific files. No two agents touch the same file.
- Agents must read their target file FIRST. Do not replace existing code with placeholders or stub it out.
- Agents must NOT run `brokkr`, `cargo`, or `./scripts/diff_test.sh`. The orchestrator validates between agents.
- Include `CLAUDE.md` (and any other top-level docs they'll need, e.g. `LLM.md`) in every agent's required reading.

**Audit protocol:**

- Do not trust agent claims of completion. Verify existence + wiring + behavior.
- Use the 3-pass audit structure: domain-specific verification, then cross-cutting reconciliation (does the new instruction actually dispatch? is the new builtin actually installed by `open_libs`?), then editorial normalization.
- Any discrepancies doc should contain only current gaps, not historical records. Remove resolved items entirely.

## 'brokkr' benchmarking

Dev tool at `~/Programs/brokkr`, invoked as `brokkr` from the project root
(reads `./brokkr.toml`). Besides `check`/`test`, it owns dellingr's
benchmarking. Workloads are the content-addressed `[dellingr.workloads.*]`
registrations in brokkr.toml, each pinning TWO files (harness =
`examples/hotpath.rs` - see AGENTS.md "Hotpath benchmarks" for that layer):
`file`/`xxh128` is the seconds-scale `bench/` script `--bench` resolves;
`hotpath_file`/`hotpath_xxh128` is the ms-scale `examples/` counterpart
`--hotpath`/`--alloc` resolve. The instrumented modes REFUSE a workload
without the pair: hotpath's per-call event queue is unbounded, and a
seconds-scale file under instrumentation backlogs 30+ GB of RAM (few-GB RSS
peaks on ms-scale files are normal). The same math governs
`scripts/hotbench.sh` and any manual `--features hotpath` run - point them
at examples/ kernels, never at `bench/` files. Editing a registered script requires
updating its pin's hash in brokkr.toml; brokkr refuses on hash drift, naming
the registration to re-register. Digests are xxh128: `xxhsum -H2 <file>`.

```
brokkr dellingr --lua <W>                    # run once, print timing, store nothing
brokkr dellingr --lua <W> --bench            # 3 runs, store in DB + sidecar
brokkr dellingr --lua <W> --hotpath          # function-level timing (hotpath feature)
brokkr dellingr --lua <W> --alloc            # allocation tracking (hotpath-alloc)
brokkr dellingr --lua <W> --bench --commit <ref>   # baseline an old commit
```

- `--bench N` runs N times, stores best (default 3); the sidecar stores
  EVERY iteration (`brokkr sidecar <uuid> --run N|all`).
- `--bench` trusts walls; `--hotpath`/`--alloc` walls are distorted by
  instrumentation - read their distributions (call counts, % of total),
  never their absolute times.
- `--commit <ref>` builds the harness at the old commit in brokkr's own
  persistent worktree (own CARGO_TARGET_DIR; `brokkr clean --worktrees`),
  but loads the workload file from the CURRENT tree, hash-checked - the
  baseline varies the VM and holds the workload fixed.
- Requires a clean git tree (except `*.md` and `.brokkr/results.db`);
  `--force` runs anyway but stores no results row. `brokkr sidecar dirty`
  reaches the latest forced/failed run's sidecar data.

Querying results (`.brokkr/results.db`):

- `brokkr results` - last 20. `brokkr results <uuid>` - one result, with
  per-iteration walls. **When the walls disagree, never read the row as a
  single number** - iteration 1 slow then 2/3 fast is a cold page cache.
- `brokkr results [--commit X] [--compare A B] [--command CMD] [--mode M]
  [--grep STR] [--grep-v STR] [-n N] [--top N]` - SQLite queries.
  `--grep`/`--grep-v` match `cli_args`+`brokkr_args`, repeatable;
  `--grep-v` is how you select the arm of an A/B distinguished only by an
  absent flag.
- `brokkr sidecar <uuid> [--human]` - per-phase summary (PARSE / SETUP /
  COLD / WARM segments with RSS, faults, core split). Read this before
  calling any delta a regression: SETUP is the script's standalone footer,
  WARM is the verdict phase.
- `brokkr sidecar <uuid> --durations [--human]` - START/END span timings;
  repeated spans collapse to count/total/min/avg/max. The WARM_BLOCK
  collapsed row's min/max IS the within-process spread - the per-run
  health check.
- `--counters` (harness heap/object brackets), `--markers`, `--samples`,
  `--stat FIELD` (compose with `--phase`/`--range`/`--where`) also exist.
  `--stalls` is inert for dellingr (single-threaded, no `*_wait_ns`
  counters).
- `brokkr sidecar --compare <A> <B>` - phase-aligned delta; annotates host
  differences (memory/governor/kernel) between the runs. Check those
  first.

### Benchmarking rules

- **Never run benchmark or profiling commands in parallel.** One at a
  time, wait for completion.
- **Interleave matched A/B cells.** `--bench N` is best-of-N within one
  cell and cannot cancel drift between cells. Alternate cells, compare
  medians, check sign consistency across pairs.
- **A same-day matched pair beats any historical number.** When they
  disagree, retire the historical figure.
- Launch-to-launch wall noise is the dominant band for a CPU-bound VM
  (sibling projects measured ~7% on byte-identical binaries; dellingr's
  own band is not yet characterized). Treat small single-digit-% wall
  deltas as noise until it is; the five-launch same-binary control is
  cheap - run it before trusting a small delta.
- Performance numbers in markdown must include commit hash and hostname.

## Rules

### General rules

- Don't use gremlins! Em-dash, en-dash, strange quotes, whatever - they're all verboten.
- Don't remind the user of CLAUDE.md rules. They wrote them, so they know them.

### Memory rules

Do not use your Memory functionality. Do not read, write, or update memories. Do not suggest saving things to memory. Durable context belongs in CLAUDE.md or the relevant docs, not in per-session memory files - this project is developed across several hosts and users, and memory does not transfer between them; CLAUDE.md does.

### Bash rules

- Each Bash invocation runs exactly one command. To run several, send multiple Bash calls (in parallel when independent). This subsumes `&&`, `;`, `|`, and multi-line scripts in one Bash call.
- Never use `sed`, `find`, `awk`, `head`, `tail`, or complex bash commands.
- Never chain commands with `&&`.
- Never chain commands with `;`.
- Never chain/pipe commands with `|`. Exception: piping into `review` is allowed (writing scratch prompt files is wasteful).
- Never capture stdout into env vars (`UUID=$(...)`).
- Never read or write from `/tmp`. All data lives in the project.
- Never run raw `cargo`, `curl`, `pkill`. Use `brokkr`.
- Never run `git` with `-C <path>`. Run `git` from the current working directory.

### git commit rules

- Always run `brokkr fmt` before a commit.
- Never commit markdown changes and/or `.brokkr/results.db` alone. Bundle them with upcoming code commits.
- When committing other changes: always tag along markdown files and `.brokkr/results.db` if dirty. (`sidecar.db` stays out of git - way too large - which is why `.gitignore` un-ignores only `results.db` from `.brokkr/`.)
- Write substantive engineering-focused commit messages.
- Has `Cargo.lock` changed? Commit it.
- Never `git push` unless the user explicitly asks. Stop after the commit.
- Remember to update CHANGELOG.md for relevant commits (but not general small performance improvements.)
