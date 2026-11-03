#!/usr/bin/env python3
"""A PR seed is removed only by retiring it (carrick-cloud#2880).

Usage: seed-retirement-check.py <base seeds.json> <head seeds.json>

Every seed name in the base file must still be in the head file, and a seed
that carries `retired` must give a non-empty `date` and `reason`. Deleting a
seed outright, or renaming it, fails: the blueprint table keeps a row for a
retired seed ("retired <date>: <reason>") and has none for a deleted one.
Standard library only.
"""
import json
import sys


def seeds(path):
    with open(path) as f:
        return {s["name"]: s for s in json.load(f)["seeds"]}


base, head = seeds(sys.argv[1]), seeds(sys.argv[2])
errors = []
for name in base:
    if name not in head:
        errors.append(f"seed {name} was deleted: retire it with retired: {{date, reason}} instead")
for name, seed in head.items():
    retired = seed.get("retired")
    if retired is not None:
        if not (isinstance(retired, dict) and set(retired) == {"date", "reason"}
                and all(isinstance(v, str) and v.strip() for v in retired.values())):
            errors.append(f"seed {name}: retired is exactly {{date, reason}}, both text")
for e in errors:
    print(f"::error::{e}")
sys.exit(1 if errors else 0)
