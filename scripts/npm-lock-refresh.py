#!/usr/bin/env python3
"""Refresh the npm lockfile's platform-package entries to a published version.

release-please bumps the lockfile's versions but cannot know the `resolved` URL
and `integrity` of packages that exist only once they are published, so after
each publish `npm ci` fails on main until those five entries are refreshed
(carrick#3789). Only `node_modules/@carrick-tools/cli-*` entries change.

    python3 scripts/npm-lock-refresh.py <version> [lockfile]

Tests: scripts/npm-lock-refresh.test.py. Read by .github/workflows/release.yml
(job `lock-refresh`).
"""
import json
import pathlib
import subprocess
import sys

PREFIX = "node_modules/@carrick-tools/cli-"
DEFAULT_LOCK = pathlib.Path(__file__).resolve().parent.parent / "npm" / "carrick" / "package-lock.json"


def fail(message):
    print(f"::error::{message}")
    sys.exit(1)


def npm_view(name, version):
    """(tarball url, integrity) the registry serves for name@version."""
    out = subprocess.run(
        ["npm", "view", f"{name}@{version}", "dist.tarball", "dist.integrity", "--json"],
        capture_output=True, text=True)
    if out.returncode != 0:
        fail(f"npm view {name}@{version} failed: {out.stderr.strip()[:300]}")
    dist = json.loads(out.stdout)
    return dist.get("dist.tarball", ""), dist.get("dist.integrity", "")


def refresh(lock, version, lookup=npm_view):
    """Point every platform entry at `version`, with the registry's URL and hash."""
    entries = [k for k in lock["packages"] if k.startswith(PREFIX)]
    if not entries:
        fail("the lockfile has no @carrick-tools/cli-* entries to refresh")
    for key in entries:
        name = key[len("node_modules/"):]
        tarball, integrity = lookup(name, version)
        short = name.split("/")[1]
        if not tarball.endswith(f"/{short}-{version}.tgz"):
            fail(f"{name}@{version}: the registry's tarball is {tarball!r}")
        if not integrity.startswith("sha"):
            fail(f"{name}@{version}: the registry gave no integrity hash")
        lock["packages"][key].update(version=version, resolved=tarball, integrity=integrity)
    return lock


def dump(lock):
    """The formatting npm itself writes, so the diff is only the five entries."""
    return json.dumps(lock, indent=2) + "\n"


if __name__ == "__main__":
    if len(sys.argv) not in (2, 3):
        sys.exit(__doc__)
    path = pathlib.Path(sys.argv[2]) if len(sys.argv) == 3 else DEFAULT_LOCK
    path.write_text(dump(refresh(json.loads(path.read_text()), sys.argv[1])))
