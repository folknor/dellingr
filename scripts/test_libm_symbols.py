"""Fixture tests for the nm symbol adapter; no build is needed."""

import unittest

from libm_symbols import forbidden_symbols


class Symbols(unittest.TestCase):
    def test_imports_versions_and_weak_definitions(self):
        self.assertEqual(
            forbidden_symbols(" U sin@GLIBC_2.2.5\n U powf\n00010 W fma\n U __log_finite\n"),
            ["__log_finite", "fma", "powf", "sin"],
        )

    def test_exact_operations_and_rust_names(self):
        self.assertEqual(
            forbidden_symbols(" U fmod\n U sqrt\n U floor\n00010 T _RNvCs_crmath_sin\n"),
            [],
        )

    def test_float_long_double_and_macho(self):
        self.assertEqual(
            forbidden_symbols(" U _cos\n U erfcf\n U lgammal\n U sincos\n"),
            ["_cos", "erfcf", "lgammal", "sincos"],
        )

    def test_private_lto_clone(self):
        self.assertEqual(forbidden_symbols("00010 t sin.llvm.123\n"), ["sin"])


if __name__ == "__main__":
    result = unittest.TextTestRunner().run(unittest.defaultTestLoader.loadTestsFromTestCase(Symbols))
    if not result.wasSuccessful():
        raise SystemExit(1)
    print("ok")
