#!/usr/bin/env python3
"""Keeps the inventory of places where Rust calls back into Lua current.

Every call made from Rust runs on the native stack, so the native-stack
envelope (docs/limits.md, "Native stack") depends on every compiled caller
path that can end in one. reference/native-calls.md describes the guard;
reference/native-calls.toml is the inventory this script checks the source
against. Run with no arguments; it prints `ok` when everything holds, and
one line per problem otherwise.

    python3 scripts/native_calls.py           # check
    python3 scripts/native_calls.py sites     # list every call site to an
                                              # inventoried function

The model is lexical, not a compiler call graph. It finds `fn` items and
their bodies (closure bodies count as part of the enclosing function, and a
library callback written as a closure in a registration list, `("name",
|state| { ... })`, counts as a function of its own named `lib:name`), skips
test code, and resolves a call by its name and receiver: `self.f(` inside
`impl T` and `Self::f(` resolve to `T::f`, `state.f(` to `State::f`,
`T::f(` to `T::f`, a bare or module-path `f(` to a free function, and any
other receiver to every method named `f`. That is the direct-call convention
reference/native-calls.md asks re-entering functions to follow.
"""

import fnmatch
import pathlib
import re
import sys
import tomllib

ROOT = pathlib.Path(__file__).resolve().parent.parent
SRC = ROOT / "src"
INVENTORY = ROOT / "reference" / "native-calls.toml"
TEST = ROOT / "tests" / "native_stack.rs"
BROKKR = ROOT / "brokkr.toml"
TEXTLINT_RULE = "native-call-marker"

# The execution boundaries: every call made from Rust goes through the first,
# and every Rust callback invocation through the second.
BOUNDARIES = [("State", "call_unguarded"), ("State", "invoke_rust_fn")]

MARKER = re.compile(r"//\s*native-call:\s*(.*?)\s*$")
# A comment that opens like a marker but does not parse as one (a missing
# colon, say). Prose that merely mentions native calls, such as a doc comment
# naming reference/native-calls.md, is not a marker attempt.
NEAR_MARKER = re.compile(r"//(?![/!])\s*native[-_]call\b")
EDGE_ID = re.compile(r"^([\w:.]+)->(\w+)(?::([\w-]+))?$")


# ---------------------------------------------------------------- lexing


def sanitize(text, keep_strings=False):
    """Blank literals and comments, keeping offsets and newlines.
    With keep_strings, retain string literals but still blank comments/chars.
    Returns the code and the line comments as (line, text)."""
    out = []
    comments = []
    i = 0
    n = len(text)
    line = 1

    def blank(s):
        return "".join("\n" if c == "\n" else " " for c in s)

    def ident_before(k):
        return k > 0 and (text[k - 1].isalnum() or text[k - 1] == "_")

    while i < n:
        c = text[i]
        if c == "\n":
            line += 1
            out.append(c)
            i += 1
            continue
        if text.startswith("//", i):
            j = text.find("\n", i)
            j = n if j < 0 else j
            comments.append((line, text[i:j]))
            out.append(blank(text[i:j]))
            i = j
            continue
        if text.startswith("/*", i):
            depth = 0
            j = i
            while j < n:
                if text.startswith("/*", j):
                    depth += 1
                    j += 2
                elif text.startswith("*/", j):
                    depth -= 1
                    j += 2
                    if depth == 0:
                        break
                else:
                    j += 1
            seg = text[i:j]
            line += seg.count("\n")
            out.append(blank(seg))
            i = j
            continue
        raw = re.match(r'(?:b|c)?r(#*)"', text[i:i + 24])
        if raw and not ident_before(i):
            hashes = raw.group(1)
            end = text.find('"' + hashes, i + raw.end())
            j = end + 1 + len(hashes)
            seg = text[i:j]
            line += seg.count("\n")
            out.append(seg if keep_strings else blank(seg))
            i = j
            continue
        if c == '"' or (c in "bc" and text.startswith('"', i + 1) and not ident_before(i)):
            j = i + (1 if c == '"' else 2)
            while j < n and text[j] != '"':
                j += 2 if text[j] == "\\" else 1
            j += 1
            seg = text[i:j]
            line += seg.count("\n")
            out.append(seg if keep_strings else blank(seg))
            i = j
            continue
        if c == "'" or (c == "b" and text.startswith("'", i + 1) and not ident_before(i)):
            k = i + (1 if c == "'" else 2)
            if k < n and text[k] == "\\":
                j = text.find("'", k + 2) + 1
                out.append(blank(text[i:j]))
                i = j
                continue
            if k + 1 < n and text[k + 1] == "'":
                out.append(blank(text[i:k + 2]))
                i = k + 2
                continue
            # A lifetime or a label.
            out.append(c)
            i += 1
            continue
        out.append(c)
        i += 1
    return "".join(out), comments


def match_brace(code, open_at):
    depth = 0
    for j in range(open_at, len(code)):
        if code[j] == "{":
            depth += 1
        elif code[j] == "}":
            depth -= 1
            if depth == 0:
                return j
    raise ValueError("unbalanced braces")


# Items compiled only for tests: `#[cfg(test)]` and `#[cfg(all(test, ...))]`.
CFG_TEST = re.compile(r"#\[cfg\((?:test|all\(test\b[^\]]*)\)\]")
IMPL = re.compile(r"\bimpl\b")
FN = re.compile(r"\bfn\s+([A-Za-z_]\w*)")
# A library callback written as a closure in a registration list.
CLOSURE_CALLBACK = re.compile(r'\(\s*"([\w.]+)"\s*,\s*\|state\|\s*\{')
CALL = re.compile(r"(\.|::|\b)([A-Za-z_]\w*)\s*(?:::\s*<[^()]*?>\s*)?\(")
NOT_CALLS = {
    "if", "while", "match", "for", "return", "fn", "loop", "in", "as", "move",
    "let", "else", "break", "continue", "where", "Some", "Ok", "Err", "None",
}


class Fn:
    def __init__(self, src, name, owner, start, body_open, body_close, sig):
        self.src = src
        self.name = name
        self.owner = owner
        self.start = start
        self.body_open = body_open
        self.body_close = body_close
        self.sig = sig
        self.line = src.line_of(start)

    @property
    def key(self):
        return (self.owner, self.name)

    @property
    def qual(self):
        return f"{self.owner}::{self.name}" if self.owner else self.name


class Source:
    def __init__(self, path):
        self.path = path
        self.rel = path.relative_to(ROOT).as_posix()
        self.text = path.read_text()
        self.lines = self.text.split("\n")
        self.code, comments = sanitize(self.text)
        self.comments = dict(comments)
        self.newlines = [i for i, c in enumerate(self.code) if c == "\n"]
        self.tests = self.test_spans()
        self.impls = self.impl_spans()
        self.fns = [f for f in self.functions() + self.closure_callbacks()
                    if not self.in_test(f.start)]

    def line_of(self, offset):
        lo, hi = 0, len(self.newlines)
        while lo < hi:
            mid = (lo + hi) // 2
            if self.newlines[mid] < offset:
                lo = mid + 1
            else:
                hi = mid
        return lo + 1

    def test_spans(self):
        spans = []
        for m in CFG_TEST.finditer(self.code):
            j = m.end()
            while j < len(self.code) and self.code[j] not in "{;":
                j += 1
            if j < len(self.code) and self.code[j] == "{":
                j = match_brace(self.code, j)
            spans.append((m.start(), j + 1))
        return spans

    def in_test(self, offset):
        return any(a <= offset < b for a, b in self.tests)

    def impl_spans(self):
        spans = []
        for m in IMPL.finditer(self.code):
            j = m.end()
            while j < len(self.code) and self.code[j] not in "{;":
                j += 1
            if j >= len(self.code) or self.code[j] != "{":
                continue
            header = re.sub(r"\bwhere\b.*", "", self.code[m.end():j], flags=re.S)
            if " for " in header:
                target = header.split(" for ")[-1]
            else:
                target = re.sub(r"^\s*<[^{]*?>\s*(?=[A-Za-z_])", "", header)
            names = re.findall(r"[A-Za-z_]\w*", re.sub(r"<.*", "", target.strip(), flags=re.S))
            spans.append((j, match_brace(self.code, j), names[-1] if names else "?"))
        return spans

    def owner_at(self, offset):
        owner = None
        for open_at, close_at, name in self.impls:
            if open_at < offset < close_at:
                owner = name
        return owner

    def functions(self):
        fns = []
        code = self.code
        for m in FN.finditer(code):
            j = m.end()
            depth = 0
            while j < len(code):
                c = code[j]
                if c in "([":
                    depth += 1
                elif c in ")]":
                    depth -= 1
                elif depth == 0 and c in "{;":
                    break
                j += 1
            if j >= len(code) or code[j] == ";":
                continue
            close = match_brace(code, j)
            fns.append(Fn(self, m.group(1), self.owner_at(m.start()), m.start(), j, close,
                          code[m.start():j]))
        return fns

    def closure_callbacks(self):
        lib = pathlib.PurePosixPath(self.rel).stem
        prefix = "" if lib == "basic" else f"{lib}."
        out = []
        for m in CLOSURE_CALLBACK.finditer(self.text):
            open_at = m.end() - 1
            if self.code[open_at] != "{":
                continue
            close = match_brace(self.code, open_at)
            out.append(Fn(self, f"lib:{prefix}{m.group(1)}", None, m.start(), open_at, close, ""))
        return out

    def enclosing(self, offset):
        best = None
        for f in self.fns:
            if f.body_open < offset < f.body_close:
                if best is None or f.body_open > best.body_open:
                    best = f
        return best

    def calls(self):
        """Every call-shaped token outside test code, as
        (name, offset, enclosing fn, owner); `owner` is a type name, None
        for a free function, or `?` for an untyped receiver."""
        for m in CALL.finditer(self.code):
            name = m.group(2)
            if name in NOT_CALLS or self.in_test(m.start()):
                continue
            if re.search(r"\bfn\s*$", self.code[max(0, m.start(2) - 8):m.start(2)]):
                continue
            enc = self.enclosing(m.start(2))
            yield name, m.start(2), enc, self.call_owner(m, enc)

    def call_owner(self, m, enc):
        sep = m.group(1)
        start = m.start(1)
        self_owner = enc.owner if enc else None
        if sep == "::":
            seg = re.search(r"([A-Za-z_]\w*)\s*$", self.code[:start])
            seg = seg.group(1) if seg else "?"
            if seg == "Self":
                return self_owner or "?"
            return seg if seg[:1].isupper() else None
        if sep == ".":
            chain = re.search(r"([A-Za-z_][\w.]*)$", self.code[:start].rstrip())
            receiver = chain.group(1) if chain else ""
            if receiver == "self":
                return self_owner or "?"
            if receiver == "state":
                return "State"
            return "?"
        return None


def production_sources():
    sources = [Source(path) for path in sorted(SRC.rglob("*.rs"))]
    test_files = set()
    test_dirs = set()
    for source in sources:
        # Out-of-line test modules inherit their cfg throughout their subtree.
        # A suggestive filename alone never exempts production code.
        base = source.path.parent
        if source.path.stem not in ("lib", "main", "mod"):
            base = base / source.path.stem
        inline_modules = []
        for module in re.finditer(r"\bmod\s+(\w+)\s*\{", source.code):
            opening = module.end() - 1
            inline_modules.append((opening, match_brace(source.code, opening), module.group(1)))
        for m in re.finditer(r"\bmod\s+(\w+)\s*;", source.code):
            if source.in_test(m.start()):
                module_base = base
                for opening, closing, name in inline_modules:
                    if opening < m.start() < closing:
                        module_base = module_base / name
                test_files.add(module_base / (m.group(1) + ".rs"))
                test_dirs.add(module_base / m.group(1))
    return [source for source in sources
            if source.path not in test_files
            and not any(directory in source.path.parents for directory in test_dirs)]


def item_attributes(fn):
    """The contiguous outer attributes before a function's modifiers."""
    prefix = fn.src.code[:fn.start]
    prefix = re.sub(r"(?:pub(?:\([^()]*\))?\s+)?"
                    r"(?:(?:async|unsafe|const)\s+)*$", "", prefix).rstrip()
    attributes = []
    while prefix.endswith("]"):
        depth = 1
        j = len(prefix) - 2
        while j >= 0 and depth:
            if prefix[j] == "]":
                depth += 1
            elif prefix[j] == "[":
                depth -= 1
            j -= 1
        if j < 0 or prefix[j] != "#":
            break
        attributes.append(prefix[j:])
        prefix = prefix[:j].rstrip()
    return "\n".join(attributes)


# ------------------------------------------------------------ call graph


class Site:
    """One lexical call to an inventoried function."""

    def __init__(self, src, offset, caller, callee):
        self.src = src
        self.offset = offset
        self.caller = caller
        self.callee = callee
        self.line = src.line_of(offset)
        self.ids = []

    @property
    def where(self):
        return f"{self.src.rel}:{self.line}"


def resolves(fn_key, name, owner):
    fn_owner, fn_name = fn_key
    if name != fn_name:
        return False
    if owner == "?":
        return fn_owner is not None
    return owner == fn_owner


def closure(sources):
    """The caller closure of the boundaries, and every call site into it."""
    calls = [(s, name, off, enc, owner)
             for s in sources for name, off, enc, owner in s.calls()]
    reached = {key: None for key in BOUNDARIES}
    queue = list(BOUNDARIES)
    sites = []
    toplevel = []
    while queue:
        key = queue.pop()
        for s, name, off, enc, owner in calls:
            if not resolves(key, name, owner):
                continue
            if enc is None:
                toplevel.append((s, off, key))
                continue
            sites.append(Site(s, off, enc, key))
            if enc.key not in reached:
                reached[enc.key] = enc
                queue.append(enc.key)
    sites.sort(key=lambda site: (site.src.rel, site.offset))
    return reached, sites, toplevel


def definitions(sources):
    defs = {}
    for s in sources:
        for f in s.fns:
            defs.setdefault(f.key, []).append(f)
    return defs


# ------------------------------------------------------------- checking


def attach_markers(sources, sites, problems):
    """Give each site the edge ids its marker names. A marker sits on the
    line of the call's name or on the line directly above it."""
    by_line = {}
    for site in sites:
        by_line.setdefault((site.src.rel, site.line), []).append(site)
    for s in sources:
        for line, comment in s.comments.items():
            m = MARKER.search(comment)
            if not m:
                if NEAR_MARKER.match(comment.strip()):
                    problems.append(f"{s.rel}:{line}: malformed native-call marker")
                continue
            ids = [part.strip() for part in m.group(1).split(",") if part.strip()]
            if not ids:
                problems.append(f"{s.rel}:{line}: native-call marker names no edge")
                continue
            code_line = s.code.split("\n")[line - 1]
            trailing = code_line.strip() != ""
            targets = by_line.get((s.rel, line), []) if trailing else []
            if not trailing:
                targets = by_line.get((s.rel, line + 1), [])
            for edge_id in ids:
                parsed = EDGE_ID.match(edge_id)
                if not parsed:
                    problems.append(f"{s.rel}:{line}: malformed edge id {edge_id!r}")
                    continue
                caller, callee, _ = parsed.groups()
                fits = [t for t in targets
                        if t.caller.name == caller and t.callee[1] == callee]
                if not targets:
                    problems.append(
                        f"{s.rel}:{line}: marker {edge_id} is not on or directly above "
                        "a call to an inventoried function")
                elif not fits:
                    problems.append(
                        f"{s.rel}:{line}: marker {edge_id} does not match its call ("
                        + ", ".join(f"{t.caller.name}->{t.callee[1]}" for t in targets)
                        + ")")
                elif len(fits) > 1:
                    problems.append(
                        f"{s.rel}:{line}: marker {edge_id} matches several calls on one line")
                else:
                    fits[0].ids.append(edge_id)


def scan_test_lists(problems):
    """Names declared in tests/native_stack.rs: `const NAME: &[...] = &[`
    lists whose entries carry `name: "..."`."""
    text = TEST.read_text()
    code, _ = sanitize(text)
    names_code, _ = sanitize(text, keep_strings=True)
    lists = {}
    for const in ("SCENARIOS", "CHAINS", "LEAVES"):
        m = re.search(rf"const {const}: &\[\w+\] = &\[", code)
        if not m:
            problems.append(f"tests/native_stack.rs: no `const {const}` list")
            lists[const] = []
            continue
        depth = 0
        end = m.end() - 1
        for j in range(m.end() - 1, len(code)):
            if code[j] == "[":
                depth += 1
            elif code[j] == "]":
                depth -= 1
                if depth == 0:
                    end = j
                    break
        # Only fields of direct entries count; comments, Lua strings and
        # nested structs must not manufacture scenario coverage.
        names = []
        depth = 0
        for field in re.finditer(r'[{}\[\]()]|\bname\s*:', code[m.end():end]):
            token = field.group()
            if token in ("{", "[", "("):
                depth += 1
            elif token in ("}", "]", ")"):
                depth -= 1
            elif depth == 1:
                offset = m.end() + field.end()
                name = re.match(r'\s*"([\w-]+)"', names_code[offset:])
                if name:
                    names.append(name.group(1))
        if len(names) != len(set(names)):
            problems.append(f"tests/native_stack.rs: duplicate names in {const}")
        lists[const] = names
    return lists


def check_textlint(names, problems):
    config = tomllib.loads(BROKKR.read_text())
    rules = [r for r in config.get("textlint", []) if r.get("name") == TEXTLINT_RULE]
    if len(rules) != 1:
        problems.append(f"brokkr.toml: expected one [[textlint]] rule named {TEXTLINT_RULE}")
        return
    rule = rules[0]
    expected = textlint_pattern(names)
    if rule.get("pattern") != expected:
        problems.append(
            f"brokkr.toml: {TEXTLINT_RULE} pattern does not match the inventory; "
            f"expected pattern = '{expected}'")
    except_expected = [rf"\bfn\s+(?:{'|'.join(sorted(names))})\b"]
    if rule.get("except") != except_expected:
        problems.append(
            f"brokkr.toml: {TEXTLINT_RULE} except does not match the inventory; "
            f"expected except = ['{except_expected[0]}']")
    if rule.get("allow_marker") != "native-call:" or rule.get("allow_marker_above") != 1:
        problems.append(
            f"brokkr.toml: {TEXTLINT_RULE} must use allow_marker = \"native-call:\" "
            "and allow_marker_above = 1, as this script does")


def textlint_pattern(names):
    return rf"\b(?:{'|'.join(sorted(names))})\s*(?:::\s*<[^>]*>\s*)?\("


ROOT_KINDS = {"callback", "host-api", "binary"}


def check_root(fn_entry, defs, sources, problems):
    key = (fn_entry.get("owner"), fn_entry["name"])
    kind = fn_entry["root"]
    label = fn_entry["name"] if not key[0] else f"{key[0]}::{key[1]}"
    if kind not in ROOT_KINDS:
        problems.append(f"inventory: {label}: unknown root kind {kind!r}")
        return
    if not fn_entry.get("evidence"):
        problems.append(f"inventory: {label}: a root needs evidence")
    found = defs.get(key, [])
    if kind == "callback":
        if key[1].startswith("lib:"):
            return
        rust_fn = [f for f in found if re.search(
            r"\(\s*state\s*:\s*&mut\s+State\s*\)\s*->\s*(?:crate::)?Result<u8>", f.sig)]
        if not rust_fn:
            problems.append(f"inventory: {label}: a callback root must have the RustFunc signature")
        # A use as a value, not a call and not the definition: how a
        # callback reaches `invoke_rust_fn`.
        value_use = re.compile(rf"(?<![\w.]){re.escape(key[1])}\b(?!\s*[(:!])")
        referenced = any(
            not re.search(r"\bfn\s+$", s.code[max(0, m.start() - 8):m.start()])
            and not s.in_test(m.start())
            for s in sources for m in value_use.finditer(s.code))
        if not referenced:
            problems.append(f"inventory: {label}: a callback root must be referenced as a value")
    elif kind == "host-api":
        public = [f for f in found
                  if re.search(r"\bpub\s+fn\s*$", f.src.code[f.src.code.rfind("\n", 0, f.start) + 1:f.start + 2])]
        if key[0] != "State" or not public:
            problems.append(f"inventory: {label}: a host-api root must be a `pub fn` on State")
    elif kind == "binary":
        if not any(f.name == "main" and f.src.rel == "src/main.rs" for f in found):
            problems.append(f"inventory: {label}: a binary root must be src/main.rs's main")


def check():
    problems = []
    sources = production_sources()
    defs = definitions(sources)
    reached, sites, toplevel = closure(sources)
    inv = tomllib.loads(INVENTORY.read_text())

    # Timing guards stay live across the entire nested Lua invocation and
    # add native-stack frames while attributing arbitrary script work here.
    unmeasured = set(reached) | {
        (entry.get("owner"), entry["name"]) for entry in inv.get("function", [])
    } | set(BOUNDARIES) | {
        ("State", "run_invocation"), ("Frame", "run")}
    for key in sorted(unmeasured, key=lambda key: (key[0] or "", key[1])):
        for fn in defs.get(key, []):
            if re.search(r"\bhotpath\s*::\s*measure\b", item_attributes(fn)):
                problems.append(f"{fn.src.rel}:{fn.line}: {fn.qual} stays live across "
                                "native re-entry and must not use hotpath::measure")

    for s, off, key in toplevel:
        problems.append(f"{s.rel}:{s.line_of(off)}: call to {key[1]} outside any function")

    # Functions: the inventory equals the computed closure.
    fns = inv.get("function", [])
    listed = {}
    for entry in fns:
        key = (entry.get("owner"), entry["name"])
        if key in listed:
            problems.append(f"inventory: function {entry['name']} listed twice")
        listed[key] = entry
    for key in reached:
        if key not in listed:
            f = reached[key]
            where = f" ({f.src.rel}:{f.line})" if f else ""
            label = f"{key[0]}::{key[1]}" if key[0] else key[1]
            problems.append(f"inventory: re-entering function {label}{where} is not listed")
    for key in listed:
        if key not in reached:
            label = f"{key[0]}::{key[1]}" if key[0] else key[1]
            problems.append(f"inventory: {label} is listed but does not reach a call into Lua")
    if len({key[1] for key in listed}) != len(listed):
        problems.append("inventory: two inventoried functions share a name; "
                        "edge ids and markers need names to be unique")
    # The names a call can spell: closure callbacks have none.
    names = {key[1] for key in listed if not key[1].startswith("lib:")}
    callers_of = {}
    for site in sites:
        callers_of.setdefault(site.callee, []).append(site)
    for key, entry in listed.items():
        if "root" in entry:
            check_root(entry, defs, sources, problems)
        elif key in reached and key not in callers_of and key not in BOUNDARIES:
            problems.append(
                f"inventory: {entry['name']} has no callers in src and is not a classified root")

    # Markers.
    attach_markers(sources, sites, problems)
    seen = {}
    for site in sites:
        if not site.ids:
            problems.append(
                f"{site.where}: call {site.caller.name}->{site.callee[1]} has no "
                "native-call marker")
        if len(site.ids) > 1:
            problems.append(f"{site.where}: several markers for one call: {site.ids}")
        for edge_id in site.ids:
            if edge_id in seen:
                problems.append(
                    f"{site.where}: edge id {edge_id} also marks {seen[edge_id].where}")
            seen[edge_id] = site
    tagged = {}
    for edge_id, site in seen.items():
        tagged.setdefault((site.caller.name, site.callee[1]), []).append(edge_id)
    for pair, ids in tagged.items():
        untagged = [i for i in ids if EDGE_ID.match(i).group(3) is None]
        if len(ids) > 1 and untagged:
            problems.append(
                f"edges {pair[0]}->{pair[1]}: several call sites, so every id needs a "
                f":tag ({', '.join(sorted(ids))})")

    # Edges and scenarios. A scenario covers the edges on its path; chains
    # and leaves cover the edges an edge entry names them for.
    lists = scan_test_lists(problems)
    scenarios = set(lists["SCENARIOS"])
    chains = set(lists["CHAINS"])
    leaves = set(lists["LEAVES"])
    edges = {}
    for entry in inv.get("edge", []):
        edge_id = entry["id"]
        if edge_id in edges:
            problems.append(f"inventory: edge {edge_id} listed twice")
        edges[edge_id] = entry
        if edge_id not in seen:
            problems.append(f"inventory: edge {edge_id} has no marked call site")
        for name in entry.get("chains", []):
            if name not in chains:
                problems.append(f"inventory: edge {edge_id}: unknown chain {name}")
        for name in entry.get("leaves", []):
            if name not in leaves:
                problems.append(f"inventory: edge {edge_id}: unknown leaf {name}")
    for edge_id, site in seen.items():
        if edge_id not in edges:
            problems.append(f"{site.where}: marker {edge_id} names no inventory edge")

    declared = {}
    on_path = {}
    segments = inv.get("segment", {})
    used_segments = set()
    for entry in inv.get("scenario", []):
        name = entry["name"]
        if name in declared:
            problems.append(f"inventory: scenario {name} listed twice")
        declared[name] = entry
        path = []
        for item in entry.get("path", []):
            if item.startswith("@"):
                if item[1:] not in segments:
                    problems.append(f"inventory: scenario {name}: unknown segment {item}")
                    continue
                used_segments.add(item[1:])
                path.extend(segments[item[1:]])
            else:
                path.append(item)
        check_path(name, path, edges, listed, problems)
        for edge_id in path:
            on_path.setdefault(edge_id, []).append(name)
    for name in sorted(set(segments) - used_segments):
        problems.append(f"inventory: segment {name} is not used by any scenario")
    for name in sorted(scenarios - set(declared)):
        problems.append(f"tests/native_stack.rs: scenario {name} is not in the inventory")
    for name in sorted(set(declared) - scenarios):
        problems.append(f"inventory: scenario {name} is not in tests/native_stack.rs")

    for edge_id, entry in edges.items():
        covered = edge_id in on_path or entry.get("chains") or entry.get("leaves")
        excluded = entry.get("excluded")
        if covered and excluded:
            problems.append(
                f"inventory: edge {edge_id} is excluded but a scenario, chain or leaf "
                "covers it")
        elif not covered and not excluded:
            problems.append(
                f"inventory: edge {edge_id} is neither covered by a scenario, chain or "
                "leaf nor excluded with a reason")

    check_textlint(names, problems)
    for stray in textlint_strays(sources, sites, names):
        if not stray_excluded(stray):
            problems.append(
                f"{stray}: matches the textlint pattern but calls no inventoried "
                f"function; add its path to {TEXTLINT_RULE}'s exclude only if the "
                "file can never call into Lua")
    for site in sites:
        if stray_excluded(f"{site.where}:"):
            problems.append(
                f"{site.where}: call into an inventoried function in a file the "
                f"{TEXTLINT_RULE} rule excludes")
    return problems


def textlint_excludes():
    config = tomllib.loads(BROKKR.read_text())
    for rule in config.get("textlint", []):
        if rule.get("name") == TEXTLINT_RULE:
            return rule.get("exclude", [])
    return []


def stray_excluded(location):
    path = location.split(":", 1)[0]
    return any(fnmatch.fnmatch(path, glob.replace("**", "*")) for glob in textlint_excludes())


def check_path(name, path, edges, listed, problems):
    if not path:
        problems.append(f"inventory: scenario {name} has an empty path")
        return
    parsed = []
    for edge_id in path:
        if edge_id not in edges:
            problems.append(f"inventory: scenario {name}: unknown edge {edge_id}")
            return
        parsed.append(EDGE_ID.match(edge_id).groups()[:2])
    if parsed[0][0] != "call_unguarded" or parsed[-1][1] != "call_unguarded":
        problems.append(
            f"inventory: scenario {name}: a path is one native level, so it starts at "
            "call_unguarded and ends with a call into it")
    roots = {key[1]: entry["root"] for key, entry in listed.items() if "root" in entry}
    for (_, callee), (caller, _) in zip(parsed, parsed[1:]):
        if callee == caller:
            continue
        # A Rust callback is entered through a function pointer.
        if callee == "invoke_rust_fn" and roots.get(caller) in ("callback", "host-api"):
            continue
        problems.append(
            f"inventory: scenario {name}: path breaks between ->{callee} and {caller}->")


def textlint_strays(sources, sites, names):
    """Lines the textlint rule would flag that call no inventoried function:
    a same-named function of another type."""
    pattern = re.compile(textlint_pattern(names))
    definition = re.compile(rf"\bfn\s+(?:{'|'.join(sorted(names))})\b")
    site_lines = {(site.src.rel, site.line) for site in sites}
    strays = []
    for s in sources:
        for number, line in enumerate(s.code.split("\n"), 1):
            if not pattern.search(line) or definition.search(line):
                continue
            offset = sum(len(x) + 1 for x in s.code.split("\n")[:number - 1])
            if s.in_test(offset) or (s.rel, number) in site_lines:
                continue
            strays.append(f"{s.rel}:{number}: {s.lines[number - 1].strip()}")
    return strays


def list_sites():
    sources = production_sources()
    reached, sites, _ = closure(sources)
    problems = []
    attach_markers(sources, sites, problems)
    for site in sites:
        mark = ", ".join(site.ids) if site.ids else "UNMARKED"
        print(f"{site.where}\t{site.caller.qual}->{site.callee[1]}\t{mark}")
    print(f"{len(reached)} functions, {len(sites)} call sites")
    for stray in textlint_strays(sources, sites, {key[1] for key in reached}):
        print("STRAY", stray)


def main() -> int:
    if sys.argv[1:] == ["sites"]:
        list_sites()
        return 0
    if sys.argv[1:]:
        sys.exit(f"usage: {sys.argv[0]} [sites]")
    problems = check()
    for problem in problems:
        print(problem)
    if problems:
        return 1
    print("ok")
    return 0


if __name__ == "__main__":
    sys.exit(main())
