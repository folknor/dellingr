#!/usr/bin/env python3
"""Translate CORE-MATH C table initializers into Rust statics.

One-off porting aid for src/crmath. Each table is copied mechanically so that
the large constant tables never pass through hand editing:

    python3 scripts/crmath_tables.py <spec> ...

where each <spec> is

    <c-file>:<c-table-name>:<rust-name>:<kind>[:<struct-name>]

and <kind> is one of f64, u64, u128, i64, u16, struct. The Rust source is
printed on stdout. Array dimensions are counted from the initializer itself.

Translations:
- hexadecimal float literals become `hf("...")` calls, which the Rust compiler
  evaluates at compile time and rejects if not exactly representable;
- decimal literals in f64 tables get a `.0` when they lack one;
- `U128(l,h)` becomes `u128_lh(l, h)`;
- designated struct initializers `{.a = x, .b = y}` become `S { a: x, b: y }`;
- C integer suffixes (u, l, ull, ...) are dropped;
- comments are removed.

The source tree is read-only input; nothing here writes files.
"""

import re
import sys

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
        return f"{struct_name} {{ " + ", ".join(fields) + " }"
    inner = [render(item, kind, struct_name, indent + "    ") for item in node]
    if all(isinstance(item, str) for item in node) and len(", ".join(inner)) < 90:
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


def main():
    out_path = None
    struct_use = None
    sources = []
    body = []
    for spec in sys.argv[1:]:
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
        header.append(f"use crate::crmath::{{{', '.join(helpers)}}};")
    if struct_use:
        header.append(f"use {struct_use};")
    if helpers or struct_use:
        header.append("")
    output = "\n".join(header) + "\n" + text
    if out_path:
        with open(out_path, "w", encoding="utf-8") as handle:
            handle.write(output)
    else:
        sys.stdout.write(output)


if __name__ == "__main__":
    main()
