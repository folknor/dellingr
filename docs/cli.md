# CLI

```
dellingr [--limit N] [--analyze] [--quiet] [--help] [--] <file.lua>
```

Short forms: `-l`, `-a`, `-q`, `-h`.

## Output streams

- stdout: script output (`print`) and the `--analyze` report.
- stderr: the `Cost used: N` summary, the `--analyze` caveat and all errors.
  `--quiet` suppresses the cost summary.

The `--analyze` caveat states that the analysis covers static bytecode only
and excludes data-dependent native string, pattern and table work.

## Arguments

- `--` ends option parsing. A script whose name starts with `-` can be run as
  `dellingr -- -name.lua`.
- Exactly one input file is required.
- `--limit` takes a signed 64-bit integer and cannot be combined with
  `--analyze`.
- Unknown options are errors.

## Exit status

Every failure exits 1: bad arguments, unreadable file, parse error, runtime
error, budget exhaustion. The cost summary is still printed before a runtime
error is reported. The CLI is a development tool; hosts embed the library and
see typed errors through the API.
