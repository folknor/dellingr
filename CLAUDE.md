@AGENTS.md

## Multi-Agent Orchestration

**Do NOT use worktree isolation for parallel agents.** Worktrees create merge conflicts that silently drop agent work. Instead, launch agents in the same tree with strict file ownership - zero overlap.

**Why no worktrees:** Worktrees let agents work on diverged snapshots. When merging back, `git checkout --ours/--theirs` drops code, conflict markers get missed, and features end up "existing but not wired" - types/functions created but never connected to bytecode dispatch, the standard library, or call sites. This has happened in long sessions and was only caught by a rigorous 3-pass audit.

**Agent coordination rules:**

- Each agent gets exclusive ownership of specific files. No two agents touch the same file.
- Agents must read their target file FIRST. Do not replace existing code with placeholders or stub it out.
- Agents must NOT run `brokkr check`, `brokkr test`, `cargo`, or `./scripts/diff_test.sh`. The orchestrator validates between agents.
- Include `CLAUDE.md` (and any other top-level docs they'll need, e.g. `LLM.md`) in every agent's required reading.

**Audit protocol:**

- Do not trust agent claims of completion. Verify existence + wiring + behavior.
- Use the 3-pass audit structure: domain-specific verification, then cross-cutting reconciliation (does the new instruction actually dispatch? is the new builtin actually installed by `open_libs`?), then editorial normalization.
- Any discrepancies doc should contain only current gaps, not historical records. Remove resolved items entirely.

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
- Never commit markdown changes alone. Bundle them with upcoming code commits.
- When committing other changes: always tag along markdown files if dirty.
- Write substantive engineering-focused commit messages.
- Has `Cargo.lock` changed? Commit it.
- Never `git push` unless the user explicitly asks. Stop after the commit.
