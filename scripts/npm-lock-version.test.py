#!/usr/bin/env python3
"""npm/carrick/package-lock.json must agree with package.json (carrick#3676).

`npm ci` refuses a lockfile whose platform-package entries do not match
package.json's optionalDependencies. Run: python3 scripts/npm-lock-version.test.py
"""
import json
import pathlib
import unittest

DIR = pathlib.Path(__file__).resolve().parent.parent / "npm" / "carrick"
PKG = json.loads((DIR / "package.json").read_text())
LOCK = json.loads((DIR / "package-lock.json").read_text())


class LockAgreesWithPackage(unittest.TestCase):
    def test_own_version(self):
        self.assertEqual(LOCK["version"], PKG["version"])
        self.assertEqual(LOCK["packages"][""]["version"], PKG["version"])

    def test_root_optional_dependencies(self):
        self.assertEqual(
            LOCK["packages"][""]["optionalDependencies"], PKG["optionalDependencies"])

    def test_platform_entries_pin_the_declared_version(self):
        for name, version in PKG["optionalDependencies"].items():
            entry = LOCK["packages"]["node_modules/" + name]
            short = name.split("/")[1]
            self.assertEqual(entry["version"], version, name)
            self.assertTrue(
                entry["resolved"].endswith(f"/{short}-{version}.tgz"), name)


if __name__ == "__main__":
    unittest.main()
