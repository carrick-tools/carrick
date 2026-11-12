#!/usr/bin/env python3
"""Tests for scripts/release-cut-check.py. Run: python3 scripts/release-cut-check.test.py

`decide` is pure and covered rule by rule. `Collect` runs the real `collect`
against a throwaway git repo and a fake `gh` on PATH (no GitHub access).
"""
import importlib.util
import json
import os
import pathlib
import subprocess
import sys
import tempfile
import unittest

spec = importlib.util.spec_from_file_location(
    "release_cut_check", pathlib.Path(__file__).with_name("release-cut-check.py"))
rcc = importlib.util.module_from_spec(spec)
spec.loader.exec_module(rcc)

SHA = "a" * 40
OTHER = "b" * 40
PENDING = {"number": 5, "label": "pending", "merge_commit": "c" * 40}
TAGGED = {**PENDING, "label": "tagged"}

# A commit that may be cut: version PR merged under it, V untagged, no holds.
CUT = {"sha": SHA, "on_main": True, "status": "success", "cargo_version": "0.3.155",
       "npm_version": "0.3.155", "tag_commit": None, "pr": PENDING,
       "last_tag_version": "0.3.154", "held_prs": []}


def facts(**kw):
    return {**CUT, **kw}


def decision(**kw):
    return rcc.decide(facts(**kw))


class Rules(unittest.TestCase):
    def test_all_pass_cuts(self):
        d = decision()
        self.assertEqual(d["decision"], "cut")
        self.assertEqual(rcc.EXIT[d["decision"]], 0)
        self.assertEqual(d["tag"], "carrick-v0.3.155")
        self.assertEqual(d["version"], "0.3.155")
        self.assertEqual(d["pr"], 5)

    # rule 1
    def test_rule1_sha_not_hex_holds(self):
        for s in ("abc", "g" * 40, "A" * 40, "a" * 41):
            d = decision(sha=s)
            self.assertEqual(d["decision"], "hold", s)

    def test_rule1_not_on_main_holds(self):
        d = decision(on_main=False)
        self.assertEqual(d["decision"], "hold")
        self.assertTrue(any("not on main" in r for r in d["reasons"]))

    # rule 2
    def test_rule2_status_not_success_holds(self):
        for s in (None, "pending", "failure", "error"):
            d = decision(status=s)
            self.assertEqual(d["decision"], "hold", s)
            self.assertTrue(any("release-check" in r and r.startswith("hold:") for r in d["reasons"]))
        self.assertTrue(any("missing" in r for r in decision(status=None)["reasons"]))

    # rule 3
    def test_rule3_version_mismatch_cannot_check(self):
        d = decision(npm_version="0.3.154")
        self.assertEqual(d["decision"], "error")
        self.assertEqual(rcc.EXIT["error"], 2)

    def test_rule3_missing_version_cannot_check(self):
        self.assertEqual(decision(cargo_version=None)["decision"], "error")
        self.assertEqual(decision(npm_version=None)["decision"], "error")

    # rule 4, five cases
    def test_rule4_tag_exists_and_pr_tagged_is_nothing(self):
        d = decision(tag_commit=OTHER, pr=TAGGED)
        self.assertEqual(d["decision"], "nothing")
        self.assertEqual(rcc.EXIT["nothing"], 3)

    def test_rule4_tag_exists_and_no_pr_is_nothing(self):
        self.assertEqual(decision(tag_commit=OTHER, pr=None)["decision"], "nothing")

    def test_rule4_tag_at_sha_and_pending_resumes(self):
        d = decision(tag_commit=SHA, pr=PENDING)
        self.assertEqual(d["decision"], "resume")
        self.assertEqual(rcc.EXIT["resume"], 0)
        self.assertEqual(d["release_commit"], PENDING["merge_commit"])

    def test_rule4_tag_elsewhere_and_pending_holds(self):
        d = decision(tag_commit=OTHER, pr=PENDING)
        self.assertEqual(d["decision"], "hold")
        self.assertTrue(any("tagged at bbbbbbbb, not here" in r for r in d["reasons"]))

    def test_rule4_no_tag_and_no_pr_is_nothing(self):
        d = decision(tag_commit=None, pr=None)
        self.assertEqual(d["decision"], "nothing")
        self.assertTrue(any("merge the version PR first" in r for r in d["reasons"]))

    def test_rule4_nothing_wins_over_missing_status(self):
        # A commit before the version PR has no check status and nothing to cut.
        self.assertEqual(decision(tag_commit=None, pr=None, status=None)["decision"], "nothing")

    def test_rule4_resume_still_needs_status(self):
        self.assertEqual(decision(tag_commit=SHA, pr=PENDING, status=None)["decision"], "hold")

    # rule 5
    def test_rule5_version_not_newer_holds(self):
        for last in ("0.3.155", "0.3.156", "0.4.0"):
            d = decision(last_tag_version=last)
            self.assertEqual(d["decision"], "hold", last)
            self.assertTrue(any("not newer" in r for r in d["reasons"]))

    def test_rule5_compares_numbers_not_text(self):
        self.assertEqual(decision(cargo_version="0.3.100", npm_version="0.3.100",
                                  last_tag_version="0.3.99")["decision"], "cut")

    # rule 6
    def test_rule6_release_hold_pr_holds(self):
        d = decision(held_prs=[7, 9])
        self.assertEqual(d["decision"], "hold")
        self.assertTrue(any("#7, #9" in r for r in d["reasons"]))

    def test_holds_accumulate(self):
        d = decision(status="failure", held_prs=[7])
        self.assertEqual(sum(r.startswith("hold:") for r in d["reasons"]), 2)

    def test_exit_codes(self):
        self.assertEqual(rcc.EXIT, {"cut": 0, "resume": 0, "hold": 1, "error": 2, "nothing": 3})


FAKE_GH = r'''#!/usr/bin/env python3
import json, os, sys
a = sys.argv[1:]
d = os.environ["FAKE_GH_DIR"]
def out(name):
    sys.stdout.write(open(os.path.join(d, name)).read())
if a[:2] == ["api", "-X"] or a[0] == "api":
    out("statuses.json")
elif a[:2] == ["pr", "list"] and "--label" in a:
    out("held.json")
elif a[:2] == ["pr", "list"]:
    out("version-prs.json")
else:
    sys.exit("unexpected gh call: %r" % a)
'''


def git(cwd, *args):
    return subprocess.run(["git", *args], cwd=cwd, check=True, capture_output=True, text=True,
                          env={**os.environ, "GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@t",
                               "GIT_COMMITTER_NAME": "t", "GIT_COMMITTER_EMAIL": "t@t"}).stdout.strip()


class Collect(unittest.TestCase):
    """collect() against a real repo: tag carrick-v0.3.154, a version commit, a held PR."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        t = pathlib.Path(self.tmp.name)
        self.repo = t / "repo"
        self.repo.mkdir()
        self.gh = t / "gh-data"
        self.gh.mkdir()
        bindir = t / "bin"
        bindir.mkdir()
        (bindir / "gh").write_text(FAKE_GH)
        (bindir / "gh").chmod(0o755)
        r = self.repo

        def commit(msg, version):
            (r / "Cargo.toml").write_text(f'[package]\nname = "carrick"\nversion = "{version}"\n')
            (r / "npm" / "carrick").mkdir(parents=True, exist_ok=True)
            (r / "npm" / "carrick" / "package.json").write_text(json.dumps({"version": version}))
            (r / "log").write_text(msg)
            git(r, "add", "-A")
            git(r, "commit", "-qm", msg)
            return git(r, "rev-parse", "HEAD")

        git(r, "init", "-q", "-b", "main")
        self.released = commit("release 0.3.154", "0.3.154")
        git(r, "tag", "carrick-v0.3.154")
        self.held = commit("feature held", "0.3.154")
        self.version = commit("release 0.3.155", "0.3.155")
        self.cut = commit("later work", "0.3.155")
        git(r, "update-ref", "refs/remotes/origin/main", self.cut)
        self.env = {**os.environ, "PATH": f"{bindir}:{os.environ['PATH']}", "FAKE_GH_DIR": str(self.gh)}
        self.write("statuses.json", [{"context": "release-check", "state": "success"},
                                     {"context": "release-check", "state": "failure"},
                                     {"context": "other", "state": "error"}])
        self.write("held.json", [{"number": 7, "mergeCommit": {"oid": self.held}},
                                 {"number": 8, "mergeCommit": {"oid": self.released}},
                                 {"number": 9, "mergeCommit": None}])
        self.write("version-prs.json", [
            {"number": 5, "title": "chore: release carrick 0.3.155", "mergeCommit": {"oid": self.version},
             "labels": [{"name": "autorelease: pending"}]},
            {"number": 4, "title": "chore: release carrick 0.3.154", "mergeCommit": {"oid": self.released},
             "labels": [{"name": "autorelease: tagged"}]}])

    def tearDown(self):
        self.tmp.cleanup()

    def write(self, name, data):
        (self.gh / name).write_text(json.dumps(data))

    def collect(self, sha=None):
        old = (os.getcwd(), os.environ.copy())
        os.chdir(self.repo)
        os.environ.clear()
        os.environ.update(self.env)
        try:
            return rcc.collect("o/r", sha or self.cut)
        finally:
            os.chdir(old[0])
            os.environ.clear()
            os.environ.update(old[1])

    def test_collects_every_fact(self):
        f = self.collect()
        self.assertTrue(f["on_main"])
        self.assertEqual(f["status"], "success")  # newest row for the context wins
        self.assertEqual((f["cargo_version"], f["npm_version"]), ("0.3.155", "0.3.155"))
        self.assertIsNone(f["tag_commit"])
        self.assertEqual(f["pr"], {"number": 5, "label": "pending", "merge_commit": self.version})
        self.assertEqual(f["last_tag_version"], "0.3.154")
        self.assertEqual(f["held_prs"], [7])  # 8 is before the tag, 9 has no merge commit
        d = rcc.decide(f)
        self.assertEqual(d["decision"], "hold")  # PR 7 carries release-hold
        self.write("held.json", [])
        self.assertEqual(rcc.decide(self.collect())["decision"], "cut")

    def test_sha_before_version_pr_is_nothing_to_cut(self):
        f = self.collect(self.held)  # carries 0.3.154; PR 5's merge commit is not under it
        self.assertEqual(f["pr"]["number"], 4)
        self.assertEqual(rcc.decide(f)["decision"], "nothing")

    def test_version_pr_merged_after_sha_is_ignored(self):
        self.write("version-prs.json", [
            {"number": 5, "title": "chore: release carrick 0.3.155", "mergeCommit": {"oid": self.cut},
             "labels": [{"name": "autorelease: pending"}]}])
        self.assertIsNone(self.collect(self.version)["pr"])

    def test_sha_not_on_main(self):
        git(self.repo, "checkout", "-q", "-b", "side", self.released)
        (self.repo / "x").write_text("x")
        git(self.repo, "add", "-A")
        git(self.repo, "commit", "-qm", "side")
        self.assertFalse(self.collect(git(self.repo, "rev-parse", "HEAD"))["on_main"])

    def test_unknown_sha_cannot_check(self):
        with self.assertRaises(RuntimeError):
            self.collect("d" * 40)

    def test_tag_at_sha_after_cut(self):
        git(self.repo, "tag", "carrick-v0.3.155", self.cut)
        f = self.collect()
        self.assertEqual(f["tag_commit"], self.cut)
        self.assertEqual(rcc.decide(f)["decision"], "resume")


class Cli(unittest.TestCase):
    def test_json_and_exit_code(self):
        d = rcc.decide(facts(status=None))
        self.assertEqual(rcc.EXIT[d["decision"]], 1)
        out = rcc.render(d, as_json=True)
        self.assertTrue(out.splitlines()[0].startswith("decision: hold"))
        body = json.loads(out.splitlines()[-1])
        self.assertEqual(set(body), {"decision", "version", "tag", "pr", "release_commit", "last_tag"})


if __name__ == "__main__":
    unittest.main()
