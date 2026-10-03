/**
 * Unit table for `reachedOnlyOnFailure` (carrick#1796): which reads the source
 * reaches only after a response failed.
 *
 * Each case is a function body with one `READ()` marker where a body read
 * would sit. `res` is the response the walk tracks; `other` is a value it does
 * not. A case is decided `true` only when no status in 200-299 can reach the
 * marker, so every `false` row below is a read the walk must keep.
 */

import { describe, it } from 'node:test';
import * as assert from 'node:assert';
import { Node, Project, SyntaxKind } from 'ts-morph';
import { reachedOnlyOnFailure, statusesIn, testsOnPath } from '../src/failure-path.js';

const PRELUDE = `declare const res: { ok: boolean; status: number };
declare const other: { ok: boolean; status: number };
declare const attempt: number;
declare const kind: number;
declare const cached: boolean;
declare function READ(): void;
declare function log(): void;
`;

const isRes = (node: Node) => Node.isIdentifier(node) && node.getText() === 'res';

function parse(source: string): { marker: Node; boundary: Node } {
  const project = new Project({ useInMemoryFileSystem: true });
  const file = project.createSourceFile('case.ts', PRELUDE + source);
  const markers = file
    .getDescendantsOfKind(SyntaxKind.Identifier)
    .filter((id) => id.getText() === 'READ' && Node.isCallExpression(id.getParent()));
  assert.strictEqual(markers.length, 1, 'one READ() marker per case');
  const boundary = file
    .getDescendantsOfKind(SyntaxKind.FunctionDeclaration)
    .find((declaration) => declaration.getName() === 'f');
  assert.ok(boundary, 'each case declares f');
  return { marker: markers[0], boundary };
}

function decide(source: string): boolean {
  const { marker, boundary } = parse(source);
  return reachedOnlyOnFailure(marker, boundary, isRes);
}

const fn = (body: string) => `function f() {\n${body}\n}`;

const CASES: Array<[string, string, boolean]> = [
  ['after an ok branch that returns', fn('if (res.ok) { return; } READ();'), true],
  ['after an ok branch that throws', fn('if (res.ok) { throw new Error(); } READ();'), true],
  ['after an ok branch that breaks out of a loop', fn('for (;;) { if (res.ok) { break; } READ(); }'), true],
  ['after an ok branch that continues a loop', fn('for (;;) { if (res.ok) { continue; } READ(); }'), true],
  ['after an ok branch whose if and else both return', fn('if (res.ok) { if (cached) { return; } else { return; } } READ();'), true],
  ['after an ok branch that runs on', fn('if (res.ok) { log(); } READ();'), false],
  ['inside a not-ok branch', fn('if (!res.ok) { READ(); }'), true],
  ['inside the else of an ok test', fn('if (res.ok) { log(); } else { READ(); }'), true],
  ['after an else that returns from a not-ok test', fn('if (!res.ok) { log(); } else { return; } READ();'), true],
  ['after a not-ok branch that throws', fn('if (!res.ok) { throw new Error(); } READ();'), false],
  ['inside a status test at or above 400', fn('if (res.status >= 400) { READ(); }'), true],
  ['inside the same test written number first', fn('if (400 <= res.status) { READ(); }'), true],
  ['after a below-400 branch that returns', fn('if (res.status < 400) { return; } READ();'), true],
  ['inside a test for one error status', fn('if (res.status === 404) { READ(); }'), true],
  ['after an early return on 204, which lets 200 through', fn('if (res.status === 204) { return; } READ();'), false],
  ['inside a test against 200, which lets 201 through', fn('if (res.status !== 200) { READ(); }'), false],
  ['inside either of two failing statuses', fn('if (res.status >= 500 || res.status === 429) { READ(); }'), true],
  ['inside a not-ok test narrowed further', fn('if (!res.ok && res.status !== 404) { READ(); }'), true],
  ['after a retry branch whose other half is not about the response', fn('for (;;) { if (res.status >= 500 && attempt < 3) { continue; } READ(); }'), false],
  ['inside the failing arm of a conditional', fn('const x = res.ok ? 1 : READ();'), true],
  ['inside the passing arm of a conditional', fn('const x = res.ok ? READ() : 1;'), false],
  ['after an ok branch that breaks out of a switch case', fn('switch (kind) { case 1: if (res.ok) { break; } READ(); }'), true],
  ['after an ok branch inside a try, which is not read', fn('try { if (res.ok) { return; } } catch {} READ();'), false],
  ['after a test of a value that is not the response', fn('if (other.ok) { return; } READ();'), false],
  ['in a callback inside a not-ok branch', fn('if (!res.ok) { [1].forEach(() => READ()); }'), true],
  ['under a not-ok test outside the function', 'if (!res.ok) { function f() { READ(); } }', false],
];

describe('carrick#1796: reachedOnlyOnFailure', () => {
  for (const [name, source, expected] of CASES) {
    it(`${expected ? 'failure path' : 'kept'}: ${name}`, () => {
      assert.strictEqual(decide(source), expected, source);
    });
  }
});

/** Every status from 100 to 599 but the listed ones. */
const allBut = (...taken: number[]) =>
  statusesIn((status) => !taken.includes(status));
const range = (from: number, to: number) =>
  statusesIn((status) => status >= from && status <= to);

/**
 * `testsOnPath` (carrick#1834): each test of the response on the path, as
 * the statuses that take the marker's side, and whether that set is only an
 * upper bound. `undefined` admits marks a side whose set the row does not pin.
 */
const SIDES: Array<[string, string, Array<{ admits?: number[]; inexact: boolean }>]> = [
  ['a test that is not about the response is not listed', fn('if (other.ok) { READ(); }'), []],
  ['a condition with no status in it is not listed', fn('if (cached) { READ(); }'), []],
  ['a range read as one set', fn('if (res.status >= 200 && res.status < 300) { READ(); }'), [{ admits: range(200, 299), inexact: false }]],
  ['an equality written number first', fn('if (204 === res.status) { return; } READ();'), [{ admits: allBut(204), inexact: false }]],
  ['each early exit is its own side', fn('if (!res.ok) { throw new Error(); } if (res.status === 202 || res.status === 204) { return; } READ();'), [{ admits: range(200, 299), inexact: false }, { admits: allBut(202, 204), inexact: false }]],
  ['a status compared with a name it cannot read', fn('if (res.status === kind) { READ(); }'), [{ inexact: true }]],
  ['the same comparison negated', fn('if (!(res.status === kind)) { READ(); }'), [{ inexact: true }]],
  ['a status test beside a condition about something else', fn('if (res.ok && cached) { READ(); }'), [{ admits: range(200, 299), inexact: true }]],
  ['the other side of that test', fn('if (res.ok && cached) { return; } READ();'), [{ admits: range(100, 599), inexact: true }]],
];

describe('carrick#1834: testsOnPath', () => {
  for (const [name, source, expected] of SIDES) {
    it(name, () => {
      const { marker } = parse(source);
      const sides = testsOnPath(marker, undefined, isRes);
      assert.strictEqual(sides.length, expected.length, source);
      expected.forEach((want, index) => {
        assert.strictEqual(sides[index].inexact, want.inexact, `${source} side ${index}`);
        if (want.admits) {
          assert.deepStrictEqual(statusesIn(sides[index].admits), want.admits, `${source} side ${index}`);
        }
      });
    });
  }
});
