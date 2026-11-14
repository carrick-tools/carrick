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
  const block = similarLines(result, "a.ts", () => true).lines[0] ?? "";
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
