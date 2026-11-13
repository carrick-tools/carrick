#!/usr/bin/env python3
"""Tests for scripts/npm-lock-refresh.py and the CI/release wiring around it
(carrick#3789). Run: python3 scripts/npm-lock-refresh.test.py"""
import copy
import importlib.util
import json
import pathlib
import re
import unittest

ROOT = pathlib.Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location(
    "npm_lock_refresh", pathlib.Path(__file__).with_name("npm-lock-refresh.py"))
nlr = importlib.util.module_from_spec(spec)
spec.loader.exec_module(nlr)

PLATFORMS = ["darwin-arm64", "darwin-x64", "linux-arm64", "linux-x64", "win32-x64"]
RELEASE_BRANCH = "release-please--branches--main--components--carrick"


def lock(version, stale):
    """A lockfile as release-please leaves it: versions bumped, hashes not."""
    packages = {
        "": {"name": "carrick", "version": version, "optionalDependencies": {
            f"@carrick-tools/cli-{p}": version for p in PLATFORMS}},
        "node_modules/zod": {
            "version": "3.25.0", "resolved": "https://registry.npmjs.org/zod/-/zod-3.25.0.tgz",
            "integrity": "sha512-zod"},
    }
    for p in PLATFORMS:
        packages[f"node_modules/@carrick-tools/cli-{p}"] = {
            "version": stale,
            "resolved": f"https://registry.npmjs.org/@carrick-tools/cli-{p}/-/cli-{p}-{stale}.tgz",
            "integrity": f"sha512-old-{p}", "optional": True}
    return {"name": "carrick", "version": version, "lockfileVersion": 3, "packages": packages}


def registry(name, version):
    short = name.split("/")[1]
    return (f"https://registry.npmjs.org/{name}/-/{short}-{version}.tgz", f"sha512-new-{short}")


class Refresh(unittest.TestCase):
    def test_platform_entries_move_to_the_published_version(self):
        out = nlr.refresh(lock("0.3.154", "0.3.153"), "0.3.154", registry)
        for p in PLATFORMS:
            e = out["packages"][f"node_modules/@carrick-tools/cli-{p}"]
            self.assertEqual(e["version"], "0.3.154")
            self.assertEqual(e["resolved"], f"https://registry.npmjs.org/@carrick-tools/cli-{p}/-/cli-{p}-0.3.154.tgz")
            self.assertEqual(e["integrity"], f"sha512-new-cli-{p}")
            self.assertTrue(e["optional"])

    def test_every_other_entry_is_untouched(self):
        before = lock("0.3.154", "0.3.153")
        out = nlr.refresh(copy.deepcopy(before), "0.3.154", registry)
        for key, entry in before["packages"].items():
            if not key.startswith("node_modules/@carrick-tools/cli-"):
                self.assertEqual(out["packages"][key], entry, key)
        self.assertEqual(out["version"], before["version"])

    def test_already_current_is_a_no_op(self):
        once = nlr.refresh(lock("0.3.154", "0.3.153"), "0.3.154", registry)
        self.assertEqual(nlr.refresh(copy.deepcopy(once), "0.3.154", registry), once)

    def test_a_tarball_of_another_version_is_refused(self):
        wrong = lambda n, v: registry(n, "0.0.1")
        with self.assertRaises(SystemExit):
            nlr.refresh(lock("0.3.154", "0.3.153"), "0.3.154", wrong)

    def test_a_missing_integrity_is_refused(self):
        bare = lambda n, v: (registry(n, v)[0], "")
        with self.assertRaises(SystemExit):
            nlr.refresh(lock("0.3.154", "0.3.153"), "0.3.154", bare)

    def test_no_platform_entries_is_refused(self):
        with self.assertRaises(SystemExit):
            nlr.refresh({"packages": {"": {}}}, "0.3.154", registry)

    def test_dump_matches_npm_formatting(self):
        text = (ROOT / "npm/carrick/package-lock.json").read_text()
        self.assertEqual(nlr.dump(json.loads(text)), text)


class Wiring(unittest.TestCase):
    """The workflow files carry the pieces; read them as text, like auto-release's."""

    ci = (ROOT / ".github/workflows/ci.yml").read_text()
    rel = (ROOT / ".github/workflows/release.yml").read_text()

    def job(self, text, name):
        m = re.search(rf"^  {re.escape(name)}:\n((?:(?:    .*)?\n)+)", text, re.M)
        self.assertIsNotNone(m, f"job {name} missing")
        return m.group(1)

    lockwf = (ROOT / ".github/workflows/npm-lock.yml").read_text()

    def test_lock_test_does_not_gate_prs(self):
        # Main's lockfile lags by design between a publish and the refresh PR
        # merging; a gating check would fail every PR and stop the merge queue.
        self.assertNotIn("npm-lock", self.job(self.ci, "ci-result"))
        self.assertNotIn("npm-lock-version.test.py", self.ci)
        self.assertNotRegex(self.ci, r"(?m)^  npm-lock:")

    def test_lock_test_runs_on_main_push_and_the_refresh_pr_only(self):
        wf = self.lockwf
        self.assertIn("python3 scripts/npm-lock-version.test.py", wf)
        self.assertRegex(wf, r"(?m)^  push:\n    branches: \[main\]")
        self.assertRegex(wf, r"(?m)^  pull_request:")
        # Not the merge queue, and not a PR unless it is the refresh PR.
        self.assertNotIn("merge_group", wf)
        body = self.job(wf, "npm-lock")
        self.assertRegex(body, r"(?m)^    if: github\.event_name == 'push' \|\| startsWith\(github\.head_ref, 'lock-refresh-'\)$")
        self.assertIn("python3 scripts/npm-lock-version.test.py", body)

    def test_refresh_job_follows_the_publish(self):
        body = self.job(self.rel, "lock-refresh")
        self.assertRegex(body, r"needs: \[target, publish\]")
        self.assertIn("is_latest == 'true'", body)
        self.assertIn("scripts/npm-lock-refresh-pr.sh", body)
        self.assertIn("RELEASE_PLEASE_TOKEN", body)
        script = (ROOT / "scripts/npm-lock-refresh-pr.sh").read_text()
        self.assertIn("scripts/npm-lock-refresh.py", script)
        self.assertIn("chore(npm): refresh lockfile platform entries", script)

    def test_dispatch_runs_the_refresh_alone(self):
        body = self.job(self.rel, "lock-refresh-only")
        self.assertIn("inputs.lock_refresh_version != ''", body)
        self.assertIn("scripts/npm-lock-refresh-pr.sh", body)
        self.assertIn("RELEASE_PLEASE_TOKEN", body)
        self.assertNotIn("needs:", body)
        # The cut must not run on that dispatch.
        self.assertIn("inputs.lock_refresh_version == ''", self.job(self.rel, "cut"))

    def test_registry_lookup_retries_then_gives_up(self):
        class R:
            def __init__(self, code, out=""):
                self.returncode, self.stdout, self.stderr = code, out, "E404"
        good = R(0, json.dumps({"dist.tarball": "u", "dist.integrity": "sha512-x"}))
        seq = [R(1), R(1), good]
        waits = []
        got = nlr.npm_view("a/b", "1.0.0", run=lambda *a, **k: seq.pop(0), sleep=waits.append)
        self.assertEqual(got, ("u", "sha512-x"))
        self.assertEqual(len(waits), 2)
        with self.assertRaises(SystemExit):
            nlr.npm_view("a/b", "1.0.0", run=lambda *a, **k: R(1), sleep=lambda s: None)

    def test_publish_job_is_unchanged_by_the_refresh(self):
        self.assertNotIn("npm-lock-refresh", self.job(self.rel, "publish"))

    def test_refresh_tests_run_in_ci(self):
        self.assertIn("python3 scripts/npm-lock-refresh.test.py", self.ci)


if __name__ == "__main__":
    unittest.main()
