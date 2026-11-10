#!/usr/bin/env python3
"""Tests for scripts/auto-release.py `decide`. Run: python3 scripts/auto-release.test.py"""
import importlib.util
import pathlib
import unittest

spec = importlib.util.spec_from_file_location(
    "auto_release", pathlib.Path(__file__).with_name("auto-release.py"))
ar = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ar)

PASS = {"pr": {"number": 1}, "status": "success", "diff_exit": 0, "held_prs": [], "enabled": "true"}


def facts(**kw):
    return {**PASS, **kw}


class Decide(unittest.TestCase):
    def test_all_pass_enqueues(self):
        self.assertEqual(ar.decide(facts())[0], "enqueue")

    def test_status_not_success_holds(self):
        for s in (None, "pending", "failure", "error"):
            action, reasons = ar.decide(facts(status=s))
            self.assertEqual(action, "hold", s)
            self.assertTrue(any("release-check" in r for r in reasons))

    def test_diff_not_version_only_holds(self):
        action, reasons = ar.decide(facts(diff_exit=1))
        self.assertEqual(action, "hold")
        self.assertTrue(any("not version-only" in r for r in reasons))

    def test_release_hold_pr_holds(self):
        action, reasons = ar.decide(facts(held_prs=[7, 9]))
        self.assertEqual(action, "hold")
        self.assertTrue(any("#7, #9" in r for r in reasons))

    def test_diff_exit_2_holds_with_reason(self):
        action, reasons = ar.decide(facts(diff_exit=2, diff_note="unknown revision"))
        self.assertEqual(action, "hold")
        self.assertTrue(any("could not be checked (unknown revision)" in r for r in reasons))

    def test_variable_unset_is_no_action_even_when_all_pass(self):
        for v in ("", None, "false", "True"):
            self.assertEqual(ar.decide(facts(enabled=v))[0], "disabled", v)

    def test_no_release_pr_holds(self):
        self.assertEqual(ar.decide({"pr": None, "enabled": "true"})[0], "hold")


class Workflow(unittest.TestCase):
    def test_decide_step_uses_release_token(self):
        # A readied or enqueued PR made with the default token fires no
        # merge_group CI; the release token does.
        text = pathlib.Path(__file__).with_name("..").joinpath(
            ".github", "workflows", "auto-release.yml").resolve().read_text()
        step = text.split("- name: Decide and enqueue", 1)[1]
        line = next(l for l in step.splitlines() if l.strip().startswith("GH_TOKEN:"))
        self.assertIn("secrets.RELEASE_PLEASE_TOKEN", line)


if __name__ == "__main__":
    unittest.main()
