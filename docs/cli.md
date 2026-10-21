# CLI

```
dellingr [--limit N] [--analyze] [--quiet] [--help] [--] <file.lua>
```

Short forms: `-l`, `-a`, `-q`, `-h`.

## Output streams

- stdout: script output (`print`) and the `--analyze` report.
- stderr: the `Cost used: N` summary, the `--analyze` caveat and all errors.
  `--quiet` suppresses the cost summary.

If a CLI stdout write for help, `--analyze` or script `print()` encounters a
closed pipe, the CLI stops quietly with status 0. Library users of
`DefaultCallbacks` get nonpanicking writes; that infallible callback ignores
write errors. Stderr write failures do not panic; the command retains its
normal result.

The `--analyze` warning uses `CostAnalysis::caveat()` and the shared
`cli::ANALYSIS_WARNING_PREFIX`. Library hosts can use the same caveat when
presenting an analysis.

## Arguments

- `--` ends option parsing. A script whose name starts with `-` can be run as
  `dellingr -- -name.lua`.
- Exactly one input file is required.
- `--limit` takes a signed 64-bit integer and cannot be combined with
  `--analyze`.
- Unknown options are errors.

## Exit status

Bad arguments, unreadable files, parse errors, runtime errors and budget
exhaustion exit 1. The cost summary is still attempted before a runtime error
is reported. A closed stdout pipe during CLI output exits quietly with status
0. The CLI only ever exits 0 or 1 (never 124, which `timeout(1)` uses for a
timeout), so wrappers such as `scripts/diff_test.sh` can read 124 as a timeout
unambiguously. The CLI is a development tool; hosts embed the library and see typed errors
through the API.
