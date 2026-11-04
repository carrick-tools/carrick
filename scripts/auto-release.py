#!/usr/bin/env python3
"""Decide whether the open release PR may enqueue itself, and do it.

    scripts/auto-release.py [--dry-run]

Reads AUTO_RELEASE_ENABLED, GH_TOKEN and GITHUB_REPOSITORY from the environment
and needs a full-history checkout. Prints one `decision:` line and the reason
for every check; exit 0 on a decision (including hold), exit 2 when the facts
could not be read.

The release PR is enqueued only when all of these hold:
  - AUTO_RELEASE_ENABLED is 'true' (the repo variable; ships unset);
  - the release PR's base tip has commit status `release-check` = success
    (posted by the release check on the replay box);
  - scripts/release-diff-check.py <base tip> <PR head> exits 0 (version-only);
  - no PR merged since the last release tag carries the label `release-hold`.

--dry-run evaluates everything and enqueues nothing. The decision logic is
`decide`; scripts/auto-release.test.py covers it. Design: release-please opens
the PR, this script only marks it ready and puts it in the merge queue.
"""
import json
import os
import subprocess
import sys

STATUS_CONTEXT = "release-check"
HOLD_LABEL = "release-hold"
RELEASE_BRANCH = "release-please--branches--main--components--carrick"


def decide(facts):
    """facts -> (action, reasons). action is 'enqueue', 'hold' or 'disabled'.

    reasons lists every check as `ok: ...` or `hold: ...`. When every check
    passes but the variable is not 'true', the action is 'disabled'.
    """
    reasons = []
    if facts.get("pr") is None:
        return "hold", ["hold: no open release PR"]
    status = facts.get("status")
    if status == "success":
        reasons.append(f"ok: {STATUS_CONTEXT} is success on the base tip")
    else:
        reasons.append(f"hold: {STATUS_CONTEXT} on the base tip is {status or 'missing'}")
    code = facts.get("diff_exit")
    if code == 0:
        reasons.append("ok: release diff is version-only")
    elif code == 1:
        reasons.append("hold: release diff is not version-only")
    else:
        reasons.append(f"hold: release diff could not be checked ({facts.get('diff_note') or 'exit ' + str(code)})")
    held = facts.get("held_prs") or []
    if held:
        reasons.append(f"hold: merged since the last tag with {HOLD_LABEL}: " + ", ".join(f"#{n}" for n in held))
    else:
        reasons.append(f"ok: no PR merged since the last tag carries {HOLD_LABEL}")
    if any(r.startswith("hold:") for r in reasons):
        return "hold", reasons
    if facts.get("enabled") != "true":
        return "disabled", reasons + ["hold: AUTO_RELEASE_ENABLED is not 'true'"]
    return "enqueue", reasons


def run(*args, check=True):
    r = subprocess.run(args, capture_output=True, text=True)
    if check and r.returncode != 0:
        raise RuntimeError(f"{' '.join(args)}: {r.stderr.strip()}")
    return r


def gh_json(*args):
    return json.loads(run("gh", *args).stdout or "null")


def collect(repo):
    """Read the facts. Raises RuntimeError when GitHub or git cannot answer."""
    prs = gh_json("pr", "list", "-R", repo, "--head", RELEASE_BRANCH, "--state", "open",
                  "--json", "number,id,isDraft,baseRefName,headRefOid")
    if not prs:
        return {"pr": None}
    pr = prs[0]
    base = gh_json("api", f"repos/{repo}/commits/{pr['baseRefName']}")["sha"]
    statuses = gh_json("api", f"repos/{repo}/commits/{base}/statuses?per_page=100")
    # The API lists newest first; the first row for the context is the current one.
    status = next((s["state"] for s in statuses if s["context"] == STATUS_CONTEXT), None)
    run("git", "fetch", "-q", "origin", pr["baseRefName"], f"pull/{pr['number']}/head")
    diff = run("python3", "scripts/release-diff-check.py", base, pr["headRefOid"], check=False)
    note = diff.stderr.strip().splitlines()[-1] if diff.returncode == 2 and diff.stderr.strip() else ""
    tag = run("git", "describe", "--tags", "--abbrev=0", "--match", "carrick-v*", base).stdout.strip()
    since = set(run("git", "rev-list", f"{tag}..{base}").stdout.split())
    merged = gh_json("pr", "list", "-R", repo, "--state", "merged", "--label", HOLD_LABEL,
                     "--base", pr["baseRefName"], "--limit", "200", "--json", "number,mergeCommit")
    held = sorted(m["number"] for m in merged if (m.get("mergeCommit") or {}).get("oid") in since)
    return {"pr": pr, "status": status, "diff_exit": diff.returncode, "diff_note": note, "held_prs": held}


ENQUEUE = """mutation($id: ID!) {
  enqueuePullRequest(input: {pullRequestId: $id}) { mergeQueueEntry { id } }
}"""


def enqueue(repo, pr):
    if pr["isDraft"]:
        run("gh", "pr", "ready", str(pr["number"]), "-R", repo)
    run("gh", "api", "graphql", "-f", f"query={ENQUEUE}", "-f", f"id={pr['id']}")


def main(argv):
    dry = "--dry-run" in argv
    repo = os.environ.get("GITHUB_REPOSITORY", "carrick-tools/carrick")
    try:
        facts = collect(repo)
    except RuntimeError as e:
        print(f"decision: hold\nhold: could not read the facts: {e}")
        return 2
    facts["enabled"] = os.environ.get("AUTO_RELEASE_ENABLED", "")
    action, reasons = decide(facts)
    print(f"decision: {action}{' (dry run)' if dry else ''}")
    for r in reasons:
        print(r)
    if action == "enqueue" and not dry:
        try:
            enqueue(repo, facts["pr"])
        except RuntimeError as e:
            print(f"enqueue failed: {e}")
            return 2
        print(f"enqueued #{facts['pr']['number']}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
