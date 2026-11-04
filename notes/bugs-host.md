# Host-boundary findings: State API, cost analysis, CLI

Filed from the 2026-11-04 defect hunt (scopes `vm-state` and `cost-cli`). Hunt
reports are work documents and may be wrong; verify before acting.

1. An entry is removed entirely when completely resolved. No historical record
   stays here.
2. Stable IDs never change and are never reused; removal leaves a gap.
3. An entry adjudicated against, verified incorrect, or whose outcome is that no
   action is taken owes comments at the code sites it names - and, where the
   claim touches a documented contract, the relevant `reference/` or `docs/`
   page - before the entry is removed, so the finding is not hunted again.
4. Once all findings are resolved, the file gets deleted.
5. Finding IDs are never written into the code or other documents. They are
   stable only until this document is drained; the next hunt writes new ones,
   and they are never deduplicated through git history. Carry the context
   inline instead.

## HOST-014 - Every State allocates names for the whole function catalog

Scope: `vm-state`. Performance; unmeasured, so any change goes through a
registered `brokkr dellingr` workload first.

`RustFnTable::new` allocates one `Box<str>` name per catalog entry for every
State. Under the `snapshot` feature, `bind_catalog` allocates two more per entry
(`ids` and `by_id`), and `catalog_conflict` formats every id again on each
`open_libs`. All of these names are compile-time strings. The cost is per State,
so it grows with how many States a host builds (one per script or per tick, for
example).

Approaches raised so far (not exhaustive): `Cow<'static, str>` names, or a
static-name side table for catalog indices.
