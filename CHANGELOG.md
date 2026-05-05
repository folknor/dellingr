# Changelog

All notable changes to dellingr are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/) and the project adheres to
[Semantic Versioning](https://semver.org/) once 1.0 lands.

## [Unreleased]

### Performance

- String-heavy workloads run roughly 2x faster across the board. The
  string pool's interner now indexes by hash for O(1) lookup instead
  of a linear scan, chained `..` collapses into a single OP_CONCAT(n)
  instead of n-1 binary concats, and the concat buffer is pre-sized.
  On the `strings/mixed` bench the wall time dropped 99ms -> 43ms; on
  `strings/patterns` 38ms -> 30ms.
- Field reads on a stable receiver (`entity.x` accessed in a hot loop)
  now go through a per-callsite inline cache - the existing field IC
  was already doing this, but the cache machinery has been extended
  to cover `obj:method()` dispatch through metatables and `s:method()`
  dispatch through the `string` library.
- Field writes have a matching SET_FIELD inline cache; the slow path
  now also populates it on first insert.
- Table mutation no longer invalidates the field cache for unrelated
  inserts on the same table (only removes / shifts bump the table
  version, since insertions don't move existing entries' indices).

## [0.1.0] - 2026-05-05

Initial release.
