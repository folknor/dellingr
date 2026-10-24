"""Reject platform transcendental routines in a linked artifact.

Read both imports and definitions: compiler-builtins can provide weak libm
definitions locally. Exact IEEE operations (including sqrt and fmod) are fine.
Rust-mangled crmath functions do not have C libm symbol names.
"""

from pathlib import Path
import re
import subprocess
import sys


TRANSCENDENTALS = (
    "acos acosh asin asinh atan atan2 atanh cos cosh sin sinh tan tanh "
    "exp exp2 exp10 expm1 log log2 log10 log1p pow cbrt hypot "
    "erf erfc gamma lgamma lgamma_r tgamma j0 j1 jn y0 y1 yn "
    "sincos sinpi cospi tanpi asinpi acospi atanpi atan2pi "
    "exp2m1 exp10m1 log2p1 log10p1 compoundn pown powr rootn "
    "fma"
).split()
FORBIDDEN = re.compile(
    r"^_?(?:__)?(?:" + "|".join(TRANSCENDENTALS) + r")(?:f|l)?(?:_finite)?$"
)


def forbidden_symbols(listing):
    rejected = set()
    for line in listing.splitlines():
        fields = line.split()
        if not fields:
            continue
        symbol = fields[-1].split("@", 1)[0]
        # Link-time optimization may keep a private clone of a C definition.
        symbol = re.split(r"\.(?:llvm|constprop|isra)\.", symbol, maxsplit=1)[0]
        if FORBIDDEN.fullmatch(symbol):
            rejected.add(symbol)
    return sorted(rejected)


def main():
    if len(sys.argv) != 2:
        raise SystemExit("usage: libm_symbols.py <built-artifact>")
    artifact = Path(sys.argv[1])
    listings = []
    # The dynamic table remains necessary when a linker strips the static one.
    for options in ([], ["--dynamic"]):
        result = subprocess.run(
            ["nm", *options, str(artifact)], check=True,
            capture_output=True, text=True,
        )
        listings.append(result.stdout)
    if not any(listing.strip() for listing in listings):
        raise SystemExit(f"no symbols found in {artifact}; cannot verify libm contract")
    rejected = forbidden_symbols("\n".join(listings))
    if rejected:
        raise SystemExit(f"platform libm symbols in {artifact}: {', '.join(rejected)}")


if __name__ == "__main__":
    main()
