#!/usr/bin/env python3
"""Translate CORE-MATH C table initializers into Rust statics.

Reproducible generator for src/crmath. Each table is copied mechanically so that
the large constant tables never pass through hand editing:

    python3 scripts/crmath_tables.py --check
    python3 scripts/crmath_tables.py --generate
    python3 scripts/crmath_tables.py [--out=<path>] [--use=<path>] <spec> ...

where each <spec> is

    <c-file>:<c-table-name>:<rust-name>:<kind>[:<struct-name>]

and <kind> is one of f64, u64, u128, i64, u16, struct. The Rust source is
printed on stdout unless --out is given. --use adds a Rust import. Array
dimensions are counted from the initializer itself.

Translations:
- hexadecimal float literals become `hf("...")` calls, which the Rust compiler
  evaluates at compile time and rejects if not exactly representable;
- decimal literals in f64 tables get a `.0` when they lack one;
- `U128(l,h)` becomes `u128_lh(l, h)`;
- designated struct initializers `{.a = x, .b = y}` become `S { a: x, b: y }`;
- C integer suffixes (u, l, ull, ...) are dropped;
- comments are removed.

--check compares all registered tables byte-for-byte without writing.
--generate rewrites them. Both require research/core-math at CORE_MATH_COMMIT,
including clean source files. No Rust formatter or build is needed.
"""

import difflib
import re
import subprocess
import sys
from pathlib import Path

CORE_MATH_COMMIT = "0ce0cbc15935e24c656280e5a46c34e89f301b7a"
ROOT = Path(__file__).resolve().parent.parent

# Output file -> (extra import, ordered source/name/Rust-name/kind/struct specs).
TABLES = {
    "asin.rs": (None, [
        "asin/asin.c:cc:ASIN_CC:f64",
        "asin/asin.c:s:ASIN_S:f64",
        "asin/asin.c:db:ASIN_DB:f64",
        "acos/acos.c:cc:ACOS_CC:f64",
        "acos/acos.c:s:ACOS_S:f64",
    ]),
    "atan.rs": (None, [
        "atan/atan.c:A:A:f64",
        "atan/atan.c:c:C:u16",
        "atan/atan.c:db:DB:f64",
    ]),
    "atan2.rs": ("crate::crmath::atan2::Tint", [
        "atan2/atan2.c:P:P:struct:Tint",
        "atan2/atan2.c:Q:Q:struct:Tint",
        "atan2/atan2.c:T2:T2:f64",
        "atan2/atan2.c:f2:F2:f64",
        "atan2/atan2.c:O:O:f64",
    ]),
    "exp.rs": (None, [
        "exp/exp.c:db:EXP_DB:f64",
        "exp/exp.c:t0:T0:f64",
        "exp/exp.c:t1:T1:f64",
    ]),
    "log.rs": ("crate::crmath::dint::Dint", [
        "log/log.c:_INVERSE:INVERSE:f64",
        "log/log.c:_LOG_INV:LOG_INV:f64",
        "log/log.c:P:P:f64",
        "log/dint.h:_INVERSE_2:INVERSE_2:struct:Dint",
        "log/dint.h:_LOG_INV_2:LOG_INV_2:struct:Dint",
        "log/dint.h:P_2:P_2:struct:Dint",
    ]),
    "log10.rs": ("crate::crmath::dint::Dint", [
        "log10/log10.c:_INVERSE:INVERSE:f64",
        "log10/log10.c:_LOG_INV:LOG_INV:f64",
        "log10/log10.c:P:P:f64",
        "log10/log10.c:pow10:POW10:f64",
        "log10/dint.h:_INVERSE_2:INVERSE_2:struct:Dint",
        "log10/dint.h:_LOG_INV_2:LOG_INV_2:struct:Dint",
        "log10/dint.h:P_2:P_2:struct:Dint",
    ]),
    "pow.rs": ("crate::crmath::dint::Dint", [
        "pow/pow.h:_INVERSE:INVERSE:f64",
        "pow/pow.h:_LOG_INV:LOG_INV:f64",
        "pow/pow.h:T1:T1:f64",
        "pow/pow.h:T2:T2:f64",
        "pow/pow.h:P_1:P_1:f64",
        "pow/pow.h:Q_1:Q_1:f64",
        "pow/dint.h:_INVERSE_2_1:INVERSE_2_1:struct:Dint",
        "pow/dint.h:_INVERSE_2_2:INVERSE_2_2:struct:Dint",
        "pow/dint.h:_LOG_INV_2_1:LOG_INV_2_1:struct:Dint",
        "pow/dint.h:_LOG_INV_2_2:LOG_INV_2_2:struct:Dint",
        "pow/dint.h:T1_2:T1_2:struct:Dint",
        "pow/dint.h:T2_2:T2_2:struct:Dint",
        "pow/dint.h:P_2:P_2:struct:Dint",
        "pow/dint.h:Q_2:Q_2:struct:Dint",
    ]),
    "pow_qint.rs": ("crate::crmath::qint::Qint", [
        "pow/qint.h:_INVERSE_3_1:INVERSE_3_1:struct:Qint",
        "pow/qint.h:_INVERSE_3_2:INVERSE_3_2:struct:Qint",
        "pow/qint.h:_LOG_INV_3_1:LOG_INV_3_1:struct:Qint",
        "pow/qint.h:_LOG_INV_3_2:LOG_INV_3_2:struct:Qint",
        "pow/qint.h:T1_3:T1_3:struct:Qint",
        "pow/qint.h:T2_3:T2_3:struct:Qint",
        "pow/qint.h:P_3:P_3:struct:Qint",
        "pow/qint.h:Q_3:Q_3:struct:Qint",
    ]),
    "sin.rs": (None, [
        "sin/sin.c:_T:T:u64",
        "sin/sin.c:PS:PS:u128",
        "sin/sin.c:PC:PC:u128",
        "sin/sin.c:U1:U1:f64",
        "sin/sin.c:U2:U2:f64",
        "sin/sin.c:S1u:S1U:u128",
        "sin/sin.c:S2u:S2U:u128",
        "sin/sin.c:C2u:C2U:u128",
    ]),
    "trig.rs": ("crate::crmath::dint::Dint", [
        "cos/cos.c:T:T:u64",
        "cos/cos.c:S:S:struct:Dint",
        "cos/cos.c:C:C:struct:Dint",
        "cos/cos.c:PSfast:PS_FAST:f64",
        "cos/cos.c:PCfast:PC_FAST:f64",
        "cos/cos.c:PS:PS:struct:Dint",
        "cos/cos.c:PC:PC:struct:Dint",
        "cos/cos.c:SC:SC:f64",
        "tan/tan.c:Tinv:TINV:u64",
    ]),
}

HEX_FLOAT = re.compile(r"[-+]?0[xX][0-9a-fA-F]*\.?[0-9a-fA-F]*[pP][-+]?[0-9]+")
INT_SUFFIX = re.compile(r"\b((?:0[xX][0-9a-fA-F]+)|(?:[0-9]+))(?:[uU]?[lL]{1,2}[uU]?|[uU])\b")


def strip_comments(text):
    text = re.sub(r"/\*.*?\*/", " ", text, flags=re.S)
    text = re.sub(r"//[^\n]*", " ", text)
    return text


def find_initializer(source, name):
    pattern = re.compile(r"\b" + re.escape(name) + r"\s*(\[[^\]]*\]\s*)*=\s*\{")
    match = pattern.search(source)
    if match is None:
        sys.exit(f"table {name} not found")
    start = match.end() - 1
    depth = 0
    for index in range(start, len(source)):
        char = source[index]
        if char == "{":
            depth += 1
        elif char == "}":
            depth -= 1
            if depth == 0:
                return source[start : index + 1]
    sys.exit(f"unterminated initializer for {name}")


def tokenize(text):
    tokens = []
    index = 0
    while index < len(text):
        char = text[index]
        if char.isspace():
            index += 1
        elif char in "{},":
            tokens.append(char)
            index += 1
        else:
            end = index
            depth = 0
            while end < len(text):
                c = text[end]
                if c == "(":
                    depth += 1
                elif c == ")":
                    depth -= 1
                elif depth == 0 and c in "{},":
                    break
                end += 1
            tokens.append(text[index:end].strip())
            index = end
    return tokens


def parse(tokens, position=0):
    """Parse a brace group starting at tokens[position] == '{'."""
    assert tokens[position] == "{"
    items = []
    position += 1
    while True:
        token = tokens[position]
        if token == "}":
            return items, position + 1
        if token == ",":
            position += 1
            continue
        if token == "{":
            item, position = parse(tokens, position)
            items.append(item)
        else:
            items.append(token)
            position += 1


def scalar(atom, kind):
    atom = atom.strip()
    atom = INT_SUFFIX.sub(r"\1", atom)
    u128_match = re.fullmatch(r"U128\s*\(\s*([^,]+?)\s*,\s*([^)]+?)\s*\)", atom)
    if u128_match:
        return f"u128_lh({u128_match.group(1)}, {u128_match.group(2)})"
    if HEX_FLOAT.fullmatch(atom):
        return f'hf("{atom}")'
    if kind == "f64":
        if re.fullmatch(r"[-+]?[0-9]+", atom):
            return atom + ".0"
        if re.fullmatch(r"[-+]?[0-9]*\.[0-9]*([eE][-+]?[0-9]+)?", atom):
            if atom.endswith("."):
                atom += "0"
            if atom.startswith(".") or atom.startswith("-.") or atom.startswith("+."):
                atom = atom.replace(".", "0.", 1)
            return atom
        sys.exit(f"unexpected f64 atom {atom!r}")
    return atom


def render(node, kind, struct_name, indent):
    if isinstance(node, str):
        return scalar(node, kind)
    if node and all(isinstance(item, str) and item.startswith(".") for item in node):
        fields = []
        for item in node:
            field, value = item[1:].split("=", 1)
            fields.append(f"{field.strip()}: {scalar(value, 'int')}")
        return (f"{struct_name} {{\n"
                + "".join(f"{indent}    {field},\n" for field in fields)
                + f"{indent}}}")
    inner = [render(item, kind, struct_name, indent + "    ") for item in node]
    if indent and all(isinstance(item, str) for item in node) and len(", ".join(inner)) + 2 <= 60:
        return "[" + ", ".join(inner) + "]"
    return "[\n" + "".join(f"{indent}    {item},\n" for item in inner) + f"{indent}]"


def dims(node):
    if isinstance(node, str):
        return []
    if node and all(isinstance(item, str) and item.startswith(".") for item in node):
        return []
    inner = [tuple(dims(item)) for item in node]
    if len(set(inner)) != 1:
        sys.exit(f"ragged table: {set(inner)}")
    return [len(node)] + list(inner[0])


def translate(specs):
    out_path = None
    struct_use = None
    sources = []
    body = []
    for spec in specs:
        if spec.startswith("--out="):
            out_path = spec[len("--out=") :]
            continue
        if spec.startswith("--use="):
            struct_use = spec[len("--use=") :]
            continue
        parts = spec.split(":")
        c_file, c_name, rust_name, kind = parts[:4]
        struct_name = parts[4] if len(parts) > 4 else None
        with open(c_file, encoding="utf-8") as handle:
            source = strip_comments(handle.read())
        tree, _ = parse(tokenize(find_initializer(source, c_name)))
        element = {"f64": "f64", "u64": "u64", "u128": "u128", "i64": "i64", "u16": "u16"}.get(
            kind, struct_name
        )
        ty = element
        for size in reversed(dims(tree)):
            ty = f"[{ty}; {size}]"
        marker = c_file.split("src/binary64/")[-1]
        if marker not in sources:
            sources.append(marker)
        body.append(f"/// `{c_name}` from CORE-MATH `{marker}`.")
        body.append(f"pub(in crate::crmath) static {rust_name}: {ty} = {render(tree, kind, struct_name, '')};")
        body.append("")
    text = "\n".join(body)
    header = [
        "//! Constant tables copied mechanically from CORE-MATH "
        + ", ".join(f"`{s}`" for s in sources)
        + ".",
        "//!",
        "//! Generated by `scripts/crmath_tables.py`; do not edit by hand.",
        "",
    ]
    helpers = [name for name in ("hf", "u128_lh") if name + "(" in text]
    if helpers:
        if len(helpers) == 1:
            header.append(f"use crate::crmath::{helpers[0]};")
        else:
            header.append(f"use crate::crmath::{{{', '.join(helpers)}}};")
    if struct_use:
        header.append(f"use {struct_use};")
    if helpers or struct_use:
        header.append("")
    if struct_use:
        imports = sorted(line for line in header if line.startswith("use "))
        header = [line for line in header if not line.startswith("use ")]
        header[-1:-1] = imports
    output = "\n".join(header) + "\n" + text
    if out_path == "-":
        return output
    if out_path:
        with open(out_path, "w", encoding="utf-8") as handle:
            handle.write(output)
    else:
        sys.stdout.write(output)


def main():
    args = sys.argv[1:]
    if args not in (["--check"], ["--generate"]):
        if not args or any(arg.startswith("--") and not arg.startswith(("--out=", "--use=")) for arg in args):
            sys.exit("usage: crmath_tables.py --check | --generate | [--out=PATH] [--use=IMPORT] SPEC ...")
        translate(args)
        return
    checkout = ROOT / "research/core-math"
    pin = subprocess.check_output(["git", "-C", str(checkout), "rev-parse", "HEAD"], text=True).strip()
    if pin != CORE_MATH_COMMIT:
        sys.exit(f"research/core-math must be at {CORE_MATH_COMMIT}, got {pin}")
    source_paths = sorted({spec.split(":")[0] for _, specs in TABLES.values() for spec in specs})
    subprocess.run(
        ["git", "-C", str(checkout), "diff", "--exit-code", "HEAD", "--"]
        + ["src/binary64/" + path for path in source_paths],
        check=True,
    )
    directory = ROOT / "src/crmath/tables"
    expected = set(TABLES) | {"mod.rs"}
    actual = {path.name for path in directory.glob("*.rs")}
    if actual != expected:
        sys.exit(f"table registry differs from Rust files: {sorted(actual ^ expected)}")
    generated = {}
    for filename, (extra_import, specs) in TABLES.items():
        full_specs = [str(checkout / "src/binary64") + "/" + spec for spec in specs]
        options = ["--out=-"]
        if extra_import:
            options.append("--use=" + extra_import)
        output = translate(options + full_specs).encode("utf-8")
        generated[filename] = output
    generated["mod.rs"] = (
        "//! Constant tables of the CORE-MATH ports, one module per upstream source,\n"
        "//! generated by `scripts/crmath_tables.py`.\n\n"
        + "".join(f"pub(super) mod {Path(name).stem};\n" for name in TABLES)
    ).encode("utf-8")
    mismatches = []
    for filename, output in generated.items():
        destination = directory / filename
        if args == ["--generate"]:
            destination.write_bytes(output)
        elif destination.read_bytes() != output:
            mismatches.append(filename)
            sys.stderr.writelines(difflib.unified_diff(
                destination.read_text().splitlines(keepends=True),
                output.decode("utf-8").splitlines(keepends=True),
                fromfile=filename, tofile="regenerated/" + filename,
            ))
    if mismatches:
        sys.exit("generated tables differ: " + ", ".join(mismatches))
    print(f"{len(generated)} table files match CORE-MATH {CORE_MATH_COMMIT}")


if __name__ == "__main__":
    main()
