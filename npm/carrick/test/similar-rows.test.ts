import assert from "node:assert/strict";
import test from "node:test";
import type { CheckResult, SimilarFunction } from "../src/contract.ts";
import { similarLines } from "../src/hook/similar.ts";

// Each reuse row's first line is one line: signature whitespace collapsed, cut
// at 160 characters with "..."; the intent gets the same at 200.
function rowsFor(found: Partial<SimilarFunction>): string[] {
  const result = {
    repo: "/w/app",
    file: "a.ts",
    recheck: {
      new_functions: [
        {
          name: "mine",
          similar: [
            { name: "theirs", repo: "app", file: "b.ts", line: 3, end_line: 9, score: 0.9, exported: true, called_from: 1, ...found },
          ],
        },
      ],
    },
  } as unknown as CheckResult;
  const block = similarLines(result, "a.ts", "/w/app", () => true).lines[0] ?? "";
  return block.split("\n").slice(1, -1);
}

test("a multi-line signature prints on one line", () => {
  const rows = rowsFor({ signature: "(input: {\n  keyExpiresAt: Date;\n  now: number;\n}) => number" });
  assert.equal(rows.length, 1);
  assert.ok(rows[0]!.includes("`theirs(input: { keyExpiresAt: Date; now: number; }) => number`"), rows[0]);
});

test("a 300-character signature prints cut at 160 plus an ellipsis", () => {
  const rows = rowsFor({ signature: `(${"x".repeat(299)})` });
  assert.ok(rows[0]!.includes(`\`theirs(${"x".repeat(159)}...\``), rows[0]);
});

test("a short signature and intent print unchanged", () => {
  const rows = rowsFor({ signature: "(a: string) => number", intent: "Does a thing." });
  assert.ok(rows[0]!.startsWith("1. `theirs(a: string) => number` at b.ts:3"), rows[0]);
  assert.equal(rows[1], "   Does a thing.");
});

test("a long multi-line intent is collapsed and cut at 200 plus an ellipsis", () => {
  const rows = rowsFor({ intent: `${"word ".repeat(10)}\n${"y".repeat(300)}` });
  assert.equal(rows.length, 2);
  assert.ok(rows[1]!.endsWith("..."), rows[1]);
  assert.equal([...rows[1]!.trim()].length, 203);
});

// Where a row is, read from the agent's folder (the hook payload's `cwd`).
function placeOf(found: Partial<SimilarFunction>, cwd: string | null): string {
  const result = {
    repo: "/w/app",
    file: "a.ts",
    recheck: {
      new_functions: [
        {
          name: "mine",
          similar: [
            { name: "theirs", repo: "app", file: "src/text/slug.ts", line: 4, end_line: 9, score: 0.9, exported: true, called_from: 1, ...found },
          ],
        },
      ],
    },
  } as unknown as CheckResult;
  const row = similarLines(result, "a.ts", cwd, () => true).lines[0]!.split("\n")[1]!;
  return row.slice(row.indexOf(" at ") + 4, row.indexOf(" (exported"));
}

test("a row in the edited repo, listed from its root, prints the repo-relative path", () => {
  assert.equal(placeOf({}, "/w/app"), "src/text/slug.ts:4");
});

test("a row in the edited repo, listed from the folder above it, names the folder", () => {
  assert.equal(placeOf({}, "/w"), "app/src/text/slug.ts:4");
});

test("a row in another repo checked out beside it names the repo and a path from the agent's folder", () => {
  const found = { repo: "shared-lib", checkout: "/w/shared-lib" };
  assert.equal(placeOf(found, "/w/app"), "../shared-lib/src/text/slug.ts:4 in repo shared-lib");
});

test("a row in a repo with no checkout here says so", () => {
  assert.equal(
    placeOf({ repo: "shared-lib" }, "/w/app"),
    "src/text/slug.ts:4 in repo shared-lib, which is not checked out on this machine",
  );
});

test("without a folder the text is the old one", () => {
  assert.equal(placeOf({}, null), "src/text/slug.ts:4");
  assert.equal(placeOf({ repo: "shared-lib", checkout: "/w/shared-lib" }, null), "shared-lib/src/text/slug.ts:4");
});
