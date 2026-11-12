#!/usr/bin/env python3
"""The one answer to "may this commit be cut as a release".

    scripts/release-cut-check.py [--json] <sha>

Run from a checkout with full history, tags and origin/main fetched; needs
GH_TOKEN and GITHUB_REPOSITORY (default carrick-tools/carrick) for `gh`.
Read-only: it writes nothing. Plan: ~/.claude-personal/plans/release-branch-2026-11-12.md.

Rules, in order (each prints an `ok:` or `hold:` line):
  1. <sha> is 40 hex and an ancestor of origin/main.
  2. The newest `release-check` status on <sha> is success.
  3. Cargo.toml and npm/carrick/package.json carry the same version V.
  4. The merged version PR for V under <sha> decides: tag carrick-vV exists
     and the PR is tagged (or absent) = nothing to cut; tag at <sha> and the
     PR still pending = resume; tag elsewhere and pending = hold; no tag and
     no PR = nothing to cut; no tag and pending = continue.
  5. V is newer than the newest carrick-v* tag.
  6. No PR carrying `release-hold` merged in carrick-v<last>..<sha>.

Exit codes: 0 cut or resume, 1 hold, 2 could not check, 3 nothing to cut.
With --json the last output line is a JSON object for the release job.
`decide` is pure; scripts/release-cut-check.test.py covers it.
"""
import json
import os
import re
import subprocess
import sys

STATUS_CONTEXT = "release-check"
HOLD_LABEL = "release-hold"
VERSION_BRANCH = "release-please--branches--main--components--carrick"
PENDING_LABEL = "autorelease: pending"
TAGGED_LABEL = "autorelease: tagged"
TAG_PREFIX = "carrick-v"
EXIT = {"cut": 0, "resume": 0, "hold": 1, "error": 2, "nothing": 3}


def semver(v):
    return tuple(int(p) for p in v.split("."))


def decide(facts):
    """facts -> {decision, reasons, version, tag, pr, release_commit, last_tag}.

    decision is cut, resume, hold, error (could not check) or nothing.
    """
    sha = facts.get("sha") or ""
    reasons = []
    holds = []
    out = {"decision": None, "reasons": reasons, "version": None, "tag": None,
           "pr": None, "release_commit": None, "last_tag": None}

    def finish(decision):
        out["decision"] = decision
        return out

    def hold(msg):
        holds.append(msg)
        reasons.append(f"hold: {msg}")

    if not re.fullmatch(r"[0-9a-f]{40}", sha):
        hold(f"{sha or 'no sha'} is not a full 40-hex commit")
        return finish("hold")
    if not facts.get("on_main"):
        hold(f"{sha[:8]} is not on main")
        return finish("hold")
    reasons.append(f"ok: {sha[:8]} is on main")

    status = facts.get("status")
    if status == "success":
        reasons.append(f"ok: {STATUS_CONTEXT} is success on {sha[:8]}")
    else:
        hold(f"{STATUS_CONTEXT} on {sha[:8]} is {status or 'missing'}")

    cargo, npm = facts.get("cargo_version"), facts.get("npm_version")
    if not cargo or not npm or cargo != npm:
        reasons.append(f"hold: could not check: Cargo.toml version {cargo!r} and npm/carrick version {npm!r} differ or are missing")
        return finish("error")
    version = out["version"] = cargo
    tag = out["tag"] = f"{TAG_PREFIX}{version}"
    reasons.append(f"ok: version {version} in Cargo.toml and npm/carrick")

    pr = facts.get("pr")
    tag_commit = facts.get("tag_commit")
    if pr:
        out["pr"] = pr["number"]
        out["release_commit"] = pr["merge_commit"]
    if pr and pr["label"] not in ("pending", "tagged"):
        reasons.append(f"hold: could not check: version PR #{pr['number']} has neither {PENDING_LABEL} nor {TAGGED_LABEL}")
        return finish("error")
    if tag_commit:
        if not pr or pr["label"] == "tagged":
            reasons.append(f"ok: {version} is released ({tag} exists and its version PR is tagged)")
            return finish("nothing")
        if tag_commit == sha:
            reasons.append(f"ok: {tag} is at {sha[:8]} and PR #{pr['number']} is pending: resume")
            return finish("hold" if holds else "resume")
        hold(f"{version} is tagged at {tag_commit[:8]}, not here")
        return finish("hold")
    if not pr:
        reasons.append(f"ok: {sha[:8]} carries released {version}; merge the version PR first, then cut a checked commit at or after it")
        return finish("nothing")
    if pr["label"] == "tagged":
        reasons.append(f"hold: could not check: PR #{pr['number']} is tagged but {tag} does not exist")
        return finish("error")
    reasons.append(f"ok: version PR #{pr['number']} merged under {sha[:8]} and still pending")

    last = facts.get("last_tag_version")
    out["last_tag"] = f"{TAG_PREFIX}{last}" if last else None
    if last is None or semver(version) > semver(last):
        reasons.append(f"ok: {version} is newer than {last or 'no earlier tag'}")
    else:
        hold(f"{version} is not newer than the last tag {TAG_PREFIX}{last}")

    held = facts.get("held_prs") or []
    if held:
        hold(f"merged since {out['last_tag']} with {HOLD_LABEL}: " + ", ".join(f"#{n}" for n in held))
    else:
        reasons.append(f"ok: no PR merged since the last tag carries {HOLD_LABEL}")
    return finish("hold" if holds else "cut")


def run(*args, check=True):
    r = subprocess.run(args, capture_output=True, text=True)
    if check and r.returncode != 0:
        raise RuntimeError(f"{' '.join(args)}: {r.stderr.strip()}")
    return r


def gh_json(*args):
    return json.loads(run("gh", *args).stdout or "null")


def read_version(sha):
    cargo = run("git", "show", f"{sha}:Cargo.toml").stdout
    m = re.search(r'^\[package\][^\[]*?^version\s*=\s*"([^"]+)"', cargo, re.M | re.S)
    npm = json.loads(run("git", "show", f"{sha}:npm/carrick/package.json").stdout).get("version")
    return (m.group(1) if m else None), npm


def collect(repo, sha):
    """Read the facts. Raises RuntimeError when git or GitHub cannot answer."""
    facts = {"sha": sha}
    if not re.fullmatch(r"[0-9a-f]{40}", sha):
        return facts
    run("git", "cat-file", "-e", f"{sha}^{{commit}}")
    facts["on_main"] = run("git", "merge-base", "--is-ancestor", sha, "origin/main", check=False).returncode == 0
    if not facts["on_main"]:
        return facts
    statuses = gh_json("api", f"repos/{repo}/commits/{sha}/statuses?per_page=100")
    # The API lists newest first; the first row for the context is the current one.
    facts["status"] = next((s["state"] for s in statuses if s["context"] == STATUS_CONTEXT), None)
    facts["cargo_version"], facts["npm_version"] = read_version(sha)
    version = facts["cargo_version"]
    if not version or version != facts["npm_version"]:
        return facts
    tag = f"{TAG_PREFIX}{version}"
    t = run("git", "rev-parse", "-q", "--verify", f"refs/tags/{tag}^{{commit}}", check=False)
    facts["tag_commit"] = t.stdout.strip() if t.returncode == 0 else None
    prs = gh_json("pr", "list", "-R", repo, "--head", VERSION_BRANCH, "--state", "merged",
                  "--limit", "100", "--json", "number,title,labels,mergeCommit")
    facts["pr"] = None
    for p in prs:
        oid = (p.get("mergeCommit") or {}).get("oid")
        if not oid or not re.search(rf"(?<![\d.]){re.escape(version)}(?![\d.])", p["title"]):
            continue
        if run("git", "merge-base", "--is-ancestor", oid, sha, check=False).returncode != 0:
            continue
        names = {l["name"] for l in p.get("labels") or []}
        label = "pending" if PENDING_LABEL in names else "tagged" if TAGGED_LABEL in names else "other"
        facts["pr"] = {"number": p["number"], "label": label, "merge_commit": oid}
        break
    tags = run("git", "tag", "--list", f"{TAG_PREFIX}*").stdout.split()
    versions = []
    for name in tags:
        v = name[len(TAG_PREFIX):]
        if re.fullmatch(r"\d+\.\d+\.\d+", v) and v != version:
            versions.append(v)
    facts["last_tag_version"] = max(versions, key=semver) if versions else None
    facts["held_prs"] = []
    if facts["last_tag_version"]:
        since = set(run("git", "rev-list", f"{TAG_PREFIX}{facts['last_tag_version']}..{sha}").stdout.split())
        merged = gh_json("pr", "list", "-R", repo, "--state", "merged", "--label", HOLD_LABEL,
                         "--base", "main", "--limit", "200", "--json", "number,mergeCommit")
        facts["held_prs"] = sorted(m["number"] for m in merged if (m.get("mergeCommit") or {}).get("oid") in since)
    return facts


def render(d, as_json):
    lines = [f"decision: {d['decision']}"] + d["reasons"]
    if as_json:
        keys = ("decision", "version", "tag", "pr", "release_commit", "last_tag")
        lines.append(json.dumps({k: d[k] for k in keys}))
    return "\n".join(lines)


def main(argv):
    as_json = "--json" in argv
    args = [a for a in argv if a != "--json"]
    if len(args) != 1:
        print("usage: release-cut-check.py [--json] <sha>", file=sys.stderr)
        return 2
    repo = os.environ.get("GITHUB_REPOSITORY", "carrick-tools/carrick")
    try:
        facts = collect(repo, args[0])
    except RuntimeError as e:
        d = {"decision": "error", "reasons": [f"hold: could not read the facts: {e}"], "version": None,
             "tag": None, "pr": None, "release_commit": None, "last_tag": None}
        print(render(d, as_json))
        return EXIT["error"]
    d = decide(facts)
    print(render(d, as_json))
    return EXIT[d["decision"]]


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
