#!/usr/bin/env python3
"""Is the diff between two revisions only a release's version bump?

    scripts/release-diff-check.py <base> <head>

Exit 0: version-only (every changed path is a release file and only version
        values moved).
Exit 1: not version-only; the offending paths are printed, one per line.
Exit 2: could not check (unknown revision, unreadable file); the reason is
        printed to stderr.

Release files are the ones release-please writes: .release-please-manifest.json,
CHANGELOG.md, Cargo.toml, Cargo.lock, crates/*/Cargo.toml, and each
`extra-files` entry of release-please-config.json, where only the configured
jsonpaths may differ. In the Cargo files only this workspace's own version
lines may differ; that is the same rule scripts/scanner-key.sh hashes by, and
scripts/scanner-key.test.sh keeps the two in step.
"""
import fnmatch
import json
import re
import subprocess
import sys

MANIFEST = ".release-please-manifest.json"
CONFIG = "release-please-config.json"
FREE_PATHS = {"CHANGELOG.md"}
CARGO_TOML = ("Cargo.toml", "crates/*/Cargo.toml")
CARGO_LOCK = "Cargo.lock"
TOKEN = re.compile(r"\.([A-Za-z_][\w-]*)|\['([^']*)'\]|\[(\d+)\]")


class CannotCheck(Exception):
    pass


def git(*args):
    r = subprocess.run(["git", *args], capture_output=True, text=True)
    if r.returncode != 0:
        raise CannotCheck(f"git {' '.join(args)}: {r.stderr.strip()}")
    return r.stdout


def show(rev, path):
    return git("show", f"{rev}:{path}")


def strip_toml_version(text):
    out, in_pkg, done = [], False, False
    for line in text.split("\n"):
        if line.startswith("["):
            in_pkg = line.rstrip() == "[package]"
        if in_pkg and not done and re.match(r"version[ \t]*=", line):
            done = True
            continue
        out.append(line)
    return "\n".join(out)


def strip_lock_version(text):
    blocks, cur = [], []
    for line in text.split("\n"):
        if line.startswith("[[package]]") and cur:
            blocks.append(cur)
            cur = []
        cur.append(line)
    blocks.append(cur)
    out = []
    for b in blocks:
        has_src = any(re.match(r"source[ \t]*=", ln) for ln in b)
        out.extend(ln for ln in b if has_src or not re.match(r"version[ \t]*=", ln))
    return "\n".join(out)


def parse_jsonpath(expr):
    if not expr.startswith("$"):
        raise CannotCheck(f"unsupported jsonpath: {expr}")
    rest, keys = expr[1:], []
    while rest:
        m = TOKEN.match(rest)
        if not m:
            raise CannotCheck(f"unsupported jsonpath: {expr}")
        if m.group(1) is not None:
            keys.append(m.group(1))
        elif m.group(2) is not None:
            keys.append(m.group(2))
        else:
            keys.append(int(m.group(3)))
        rest = rest[m.end():]
    return keys


def blank(doc, keys):
    """Replace the value at keys with a marker; no-op when the path is absent."""
    node = doc
    for k in keys[:-1]:
        try:
            node = node[k]
        except (KeyError, IndexError, TypeError):
            return
    try:
        node[keys[-1]] = "<release>"
    except (KeyError, IndexError, TypeError):
        pass


def json_only_paths_differ(base, head, path, jsonpaths):
    a, b = json.loads(show(base, path)), json.loads(show(head, path))
    for keys in jsonpaths:
        blank(a, keys)
        blank(b, keys)
    return a != b


def manifest_keys_differ(base, head):
    a, b = json.loads(show(base, MANIFEST)), json.loads(show(head, MANIFEST))
    return a.keys() != b.keys()


def main(argv):
    if len(argv) != 3:
        print("usage: release-diff-check.py <base> <head>", file=sys.stderr)
        return 2
    base, head = argv[1], argv[2]
    for rev in (base, head):
        git("rev-parse", "--verify", "--quiet", f"{rev}^{{commit}}")
    config = json.loads(show(head, CONFIG))
    jsonpaths = {}
    for pkg in config.get("packages", {}).values():
        for ef in pkg.get("extra-files", []):
            if isinstance(ef, str):
                raise CannotCheck(f"extra-file without jsonpath: {ef}")
            if ef.get("type") != "json":
                raise CannotCheck(f"extra-file type not handled: {ef}")
            jsonpaths.setdefault(ef["path"], []).append(parse_jsonpath(ef["jsonpath"]))

    changed = git("diff", "--name-status", "--no-renames", base, head).splitlines()
    offenders = []
    for row in changed:
        status, path = row.split("\t", 1)
        if status != "M":
            offenders.append(f"{path}  (status {status})")
        elif path in FREE_PATHS:
            continue
        elif path == MANIFEST:
            if manifest_keys_differ(base, head):
                offenders.append(f"{path}  (keys changed)")
        elif path == CARGO_LOCK:
            if strip_lock_version(show(base, path)) != strip_lock_version(show(head, path)):
                offenders.append(f"{path}  (more than own version lines)")
        elif any(fnmatch.fnmatchcase(path, p) and p.count("/") == path.count("/") for p in CARGO_TOML):
            if strip_toml_version(show(base, path)) != strip_toml_version(show(head, path)):
                offenders.append(f"{path}  (more than the version line)")
        elif path in jsonpaths:
            if json_only_paths_differ(base, head, path, jsonpaths[path]):
                offenders.append(f"{path}  (more than the configured jsonpaths)")
        else:
            offenders.append(f"{path}  (not a release file)")
    for o in offenders:
        print(o)
    return 1 if offenders else 0


if __name__ == "__main__":
    try:
        sys.exit(main(sys.argv))
    except CannotCheck as e:
        print(f"release-diff-check: {e}", file=sys.stderr)
        sys.exit(2)
    except (json.JSONDecodeError, ValueError, OSError) as e:
        print(f"release-diff-check: {e}", file=sys.stderr)
        sys.exit(2)
