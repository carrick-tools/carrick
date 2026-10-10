#!/usr/bin/env python3
"""Every tests/*.rs is run by CI, exactly once.

Reads the `rust-tests` shard map in .github/workflows/ci.yml. A test binary
passes when exactly one shard names it under `targets:`, or when `m` shards
name it under `partitioned:` with `partition: slice:1/m` to `slice:m/m`. A
binary in neither shape, named twice, or named in the map without a file
behind it fails. The library target (`--lib`) must be partitioned the same
way. Standard library only, so the job needs no toolchain.
"""
import collections
import pathlib
import re
import sys

CI = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else ".github/workflows/ci.yml")
TESTS = pathlib.Path(sys.argv[2] if len(sys.argv) > 2 else "tests")
# Eval harness; needs CARRICK_API_ENDPOINT and skips without it.
EXCLUDED = {"eval_tier_a"}

text = CI.read_text()
plain = collections.defaultdict(list)  # binary -> shards naming it whole
sliced = collections.defaultdict(list)  # target -> [(shard, k, m)]

entry = re.compile(r"^ +- shard: (\S+)\n((?: {12,}\S.*\n?)*)", re.M)
for m in entry.finditer(text):
    shard, body = m.group(1), m.group(2)
    targets = re.search(r"^ +targets: >-\n((?: {14,}\S.*\n?)*)", body, re.M)
    for name in re.findall(r"--test (\w+)", targets.group(1) if targets else ""):
        plain[name].append(shard)
    part = re.search(r"^ +partitioned: (.+)$", body, re.M)
    if part:
        spec = re.search(r"^ +partition: slice:(\d+)/(\d+)$", body, re.M)
        if not spec:
            print(f"::error::shard {shard} partitions {part.group(1)} without 'partition: slice:k/m'")
            sys.exit(1)
        sliced[part.group(1).strip()].append((shard, int(spec.group(1)), int(spec.group(2))))

errors = []


def check_sliced(target):
    rows = sliced[target]
    ms = {m for _, _, m in rows}
    ks = sorted(k for _, k, _ in rows)
    if len(ms) != 1 or ks != list(range(1, next(iter(ms)) + 1)):
        errors.append(f"{target} is sliced across shards {[r[0] for r in rows]} but not as slice:1/m to slice:m/m")


files = {p.stem for p in TESTS.glob("*.rs")} - EXCLUDED
for name in sorted(files):
    whole, cut = plain.get(name, []), sliced.get(f"--test {name}", [])
    if whole and cut:
        errors.append(f"tests/{name}.rs is named whole in {whole} and sliced in {[r[0] for r in cut]}")
    elif len(whole) > 1:
        errors.append(f"tests/{name}.rs is run by more than one shard: {whole}")
    elif not whole and not cut:
        errors.append(f"tests/{name}.rs is not run by any CI shard (add it to a shard in ci.yml or list it in EXCLUDED with a reason)")
    elif cut:
        check_sliced(f"--test {name}")

if "--lib" not in sliced:
    errors.append("the library target (--lib) is not run by any shard")
else:
    check_sliced("--lib")

for name in sorted(set(plain) | {t.split(" ", 1)[1] for t in sliced if t.startswith("--test ")}):
    if name not in files and name not in EXCLUDED:
        errors.append(f"the shard map names --test {name}, which has no tests/{name}.rs")

for e in errors:
    print(f"::error::{e}")
if errors:
    sys.exit(1)
print(f"{len(files)} integration test binaries and the library are each run exactly once")
